//! Implementor-owned artifact capture (RFC-122 §1).
//!
//! An [`ArtifactSink`] is the trait the embedding application implements to
//! capture AI-produced bytes into *its* storage, returning an
//! [`ArtifactRef`] it minted. The framework never stores artifact bytes and
//! never invents a serving scheme — both are implementor authority, exactly
//! as `SessionStore` owns thread persistence and
//! [`BlobStore`](super::session::BlobStore) owns out-of-line payloads.
//!
//! Bytes flow one way: there is deliberately no `get` on the trait. Reads
//! happen through the implementor's own serving path (signed URLs, app
//! routes); a retrieval method would turn the framework into a byte proxy
//! and reopen the §D10 wire-flood problem the ref shape exists to close.
//!
//! Shipped impls, per the traits-not-bundled-stores doctrine:
//! [`MemoryArtifactSink`] (tests/dev) and [`BlobArtifactSink`] (delegates
//! bytes to any W7 `BlobStore`). [`verify_artifact_sink`] is the executable
//! conformance suite an implementor runs against its own impl.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ArtifactRef};
use pocopine_auth::Principal;

use super::session::{BlobStore, ThreadId};

/// The boxed future all [`ArtifactSink`] methods return.
pub type ArtifactFuture<'a, T> = Pin<Box<dyn Future<Output = AgenkitResult<T>> + Send + 'a>>;

/// AI-produced bytes to capture (RFC-122 §1).
#[derive(Clone, Debug)]
pub struct NewArtifact {
    /// IANA media type of the bytes.
    pub media_type: String,
    /// Optional display name.
    pub name: Option<String>,
    /// The bytes themselves. Cross exactly one boundary — into the sink.
    pub bytes: Vec<u8>,
    /// Ids of artifacts this one was derived from (RFC-122 §2.2): the source
    /// image of an edit, the still a video was animated from. Empty = fresh
    /// generation. Claims, not facts: a sink SHOULD verify each id exists
    /// and is visible to the capturing principal before recording it.
    pub derived_from: Vec<String>,
}

/// Who produced an artifact, on the capture side (RFC-122 §1). Mirrors the
/// thread-filesystem group identity: which durable producer, which slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactOrigin {
    /// Produced by a tool during dispatch.
    Tool {
        /// The provider's call id for the producing invocation.
        call_id: String,
        /// The tool's registry id.
        tool_id: String,
        /// Dense output slot within the invocation, from 0.
        output_ordinal: u32,
    },
    /// Produced by the model itself (the RFC-122 §4 backstop).
    Model,
}

/// Provenance the framework attaches to every capture (RFC-122 §1): who,
/// which thread, which producer. Built by the runtime — never by tool code.
#[derive(Clone, Debug)]
pub struct ArtifactCx {
    /// The caller the capture runs under (§D5). The sink enforces its own
    /// ownership rules against this, exactly as `SessionStore` impls do.
    pub principal: Principal,
    /// The producing thread, when the capture happens inside a session turn.
    pub thread: Option<ThreadId>,
    /// The producer.
    pub origin: ArtifactOrigin,
}

/// What a sink supports (RFC-122 §1). Mirrors
/// `pocopine_storage::StorageBackend::capabilities` — consumers consult it,
/// they don't probe by failing.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ArtifactSinkCapabilities {
    /// Largest artifact the sink accepts, if bounded. A `put` above the
    /// bound MUST error before storing.
    pub max_bytes: Option<u64>,
}

/// Implementor-owned capture of AI-produced bytes (RFC-122 §1).
///
/// Contract:
/// - The sink owns identity and addressing: `id` and `uri` are minted by the
///   implementor; the framework never parses or derives them.
/// - Digest honesty: the returned `sha256`/`len` MUST match the stored
///   bytes ([`verify_artifact_sink`] checks).
/// - A re-`put` of identical bytes under the same [`ArtifactCx`] MUST
///   succeed — idempotent or a fresh version, never an error.
/// - Principal scoping is the sink's job; the framework hands it the
///   caller's [`Principal`] and the sink applies its own policy.
pub trait ArtifactSink: Send + Sync {
    /// What this sink supports.
    fn capabilities(&self) -> ArtifactSinkCapabilities {
        ArtifactSinkCapabilities::default()
    }

    /// Store the bytes under the caller's authority and mint a reference.
    fn put<'a>(&'a self, cx: &'a ArtifactCx, artifact: NewArtifact)
    -> ArtifactFuture<'a, ArtifactRef>;
}

/// Reject `artifact` if it exceeds `max_bytes` — the shared enforcement the
/// shipped sinks use so a declared capability is honored, not advisory.
fn check_max_bytes(artifact: &NewArtifact, max_bytes: Option<u64>) -> AgenkitResult<()> {
    if let Some(max) = max_bytes
        && artifact.bytes.len() as u64 > max
    {
        return Err(AgenkitError::validation(format!(
            "artifact `{}` is {} bytes, over the sink's {} byte cap",
            artifact.name.as_deref().unwrap_or(&artifact.media_type),
            artifact.bytes.len(),
            max
        )));
    }
    Ok(())
}

/// Content-address `bytes` into a minted [`ArtifactRef`]. Shared by the
/// shipped sinks: id = sha256 hex, uri = `artifact:<sha256>`.
fn content_addressed_ref(artifact: &NewArtifact) -> ArtifactRef {
    let sha256 = pocopine_crypto::sha256_hex(&artifact.bytes);
    ArtifactRef {
        id: sha256.clone(),
        uri: Some(format!("artifact:{sha256}")),
        media_type: artifact.media_type.clone(),
        name: artifact.name.clone(),
        len: artifact.bytes.len() as u64,
        sha256,
    }
}

/// An in-memory artifact sink (tests / dev — bytes are lost on drop).
/// Content-addressed, so identical bytes dedupe to one entry.
#[derive(Default, Clone)]
pub struct MemoryArtifactSink {
    artifacts: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    max_bytes: Option<u64>,
}

impl MemoryArtifactSink {
    /// A fresh, empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare (and enforce) a byte cap.
    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = Some(max);
        self
    }

    /// The number of distinct artifacts held (post-dedup).
    pub fn len(&self) -> usize {
        self.artifacts.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether the sink holds no artifacts.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The stored bytes for `id` (test assertions — NOT part of the
    /// [`ArtifactSink`] contract, which is deliberately write-only).
    pub fn bytes(&self, id: &str) -> Option<Vec<u8>> {
        self.artifacts.lock().ok()?.get(id).cloned()
    }
}

impl ArtifactSink for MemoryArtifactSink {
    fn capabilities(&self) -> ArtifactSinkCapabilities {
        ArtifactSinkCapabilities {
            max_bytes: self.max_bytes,
        }
    }

    fn put<'a>(
        &'a self,
        _cx: &'a ArtifactCx,
        artifact: NewArtifact,
    ) -> ArtifactFuture<'a, ArtifactRef> {
        let result = check_max_bytes(&artifact, self.max_bytes).and_then(|()| {
            let reference = content_addressed_ref(&artifact);
            self.artifacts
                .lock()
                .map_err(|_| AgenkitError::internal("artifact sink mutex poisoned"))?
                .insert(reference.id.clone(), artifact.bytes);
            Ok(reference)
        });
        Box::pin(async move { result })
    }
}

/// An artifact sink over any W7 [`BlobStore`]: bytes live content-addressed
/// beside the session log, refs are minted as `artifact:<sha256>`. Enough to
/// run examples and tests durably without an app-owned storage plane.
pub struct BlobArtifactSink {
    blobs: Arc<dyn BlobStore>,
    max_bytes: Option<u64>,
}

impl BlobArtifactSink {
    /// A sink writing into `blobs`.
    pub fn new(blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            blobs,
            max_bytes: None,
        }
    }

    /// Declare (and enforce) a byte cap.
    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = Some(max);
        self
    }
}

impl ArtifactSink for BlobArtifactSink {
    fn capabilities(&self) -> ArtifactSinkCapabilities {
        ArtifactSinkCapabilities {
            max_bytes: self.max_bytes,
        }
    }

    fn put<'a>(
        &'a self,
        _cx: &'a ArtifactCx,
        artifact: NewArtifact,
    ) -> ArtifactFuture<'a, ArtifactRef> {
        Box::pin(async move {
            check_max_bytes(&artifact, self.max_bytes)?;
            let reference = content_addressed_ref(&artifact);
            self.blobs
                .put(&reference.sha256, artifact.bytes)
                .await
                .map_err(|e| AgenkitError::internal(format!("artifact blob store: {e}")))?;
            Ok(reference)
        })
    }
}

/// The executable conformance suite for an [`ArtifactSink`] impl
/// (RFC-122 §1; template: the thread store's `verify_owner_semantics`).
///
/// Checks, failing loudly with the violated rule:
/// - **Digest honesty** — the returned `sha256`/`len` match the bytes given.
/// - **Idempotent re-put** — capturing identical bytes under the same
///   [`ArtifactCx`] succeeds (same ref or a fresh version, never an error).
/// - **Cap enforcement** — a declared `max_bytes` rejects an oversized put.
///
/// A missing `uri` is legal (headless sink) but logged as a warning, since
/// every wire consumer then needs an out-of-band id-resolution story.
pub async fn verify_artifact_sink(sink: &dyn ArtifactSink) -> AgenkitResult<()> {
    let cx = ArtifactCx {
        principal: Principal::anonymous(),
        thread: None,
        origin: ArtifactOrigin::Tool {
            call_id: "conformance_call".to_string(),
            tool_id: "conformance.tool".to_string(),
            output_ordinal: 0,
        },
    };
    let bytes = b"artifact sink conformance payload".to_vec();
    let artifact = NewArtifact {
        media_type: "application/octet-stream".to_string(),
        name: Some("conformance.bin".to_string()),
        bytes: bytes.clone(),
        derived_from: Vec::new(),
    };

    let reference = sink.put(&cx, artifact.clone()).await?;
    let expected_sha = pocopine_crypto::sha256_hex(&bytes);
    if reference.sha256 != expected_sha {
        return Err(AgenkitError::validation(format!(
            "sink violates digest honesty: returned sha256 {} for bytes hashing to {}",
            reference.sha256, expected_sha
        )));
    }
    if reference.len != bytes.len() as u64 {
        return Err(AgenkitError::validation(format!(
            "sink violates length honesty: returned len {} for {} bytes",
            reference.len,
            bytes.len()
        )));
    }
    if reference.uri.is_none() {
        tracing::warn!(
            target: "pocopine.log",
            artifact_id = %reference.id,
            "artifact sink mints no uri; wire consumers must resolve by id out of band"
        );
    }

    // Idempotent re-put: same cx + bytes must not error.
    let again = sink.put(&cx, artifact).await.map_err(|e| {
        AgenkitError::validation(format!("sink rejects an identical re-put: {e}"))
    })?;
    if again.sha256 != expected_sha {
        return Err(AgenkitError::validation(
            "sink re-put returned a ref for different bytes",
        ));
    }

    // A declared cap must be enforced, not advisory.
    if let Some(max) = sink.capabilities().max_bytes {
        let over = NewArtifact {
            media_type: "application/octet-stream".to_string(),
            name: Some("oversized.bin".to_string()),
            bytes: vec![0u8; max as usize + 1],
            derived_from: Vec::new(),
        };
        if sink.put(&cx, over).await.is_ok() {
            return Err(AgenkitError::validation(format!(
                "sink declares max_bytes = {max} but accepted a larger artifact"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::session::MemoryBlobStore;

    fn cx() -> ArtifactCx {
        ArtifactCx {
            principal: Principal::anonymous(),
            thread: Some(ThreadId::new("th_1")),
            origin: ArtifactOrigin::Model,
        }
    }

    fn png(bytes: &[u8]) -> NewArtifact {
        NewArtifact {
            media_type: "image/png".to_string(),
            name: Some("out.png".to_string()),
            bytes: bytes.to_vec(),
            derived_from: Vec::new(),
        }
    }

    #[tokio::test]
    async fn memory_sink_mints_honest_content_addressed_refs() {
        let sink = MemoryArtifactSink::new();
        let reference = sink.put(&cx(), png(b"pixels")).await.unwrap();
        assert_eq!(reference.sha256, pocopine_crypto::sha256_hex(b"pixels"));
        assert_eq!(reference.len, 6);
        assert_eq!(
            reference.uri.as_deref(),
            Some(format!("artifact:{}", reference.sha256).as_str())
        );
        assert_eq!(sink.bytes(&reference.id).as_deref(), Some(&b"pixels"[..]));

        // Identical bytes dedupe; distinct bytes don't.
        sink.put(&cx(), png(b"pixels")).await.unwrap();
        assert_eq!(sink.len(), 1);
        sink.put(&cx(), png(b"other")).await.unwrap();
        assert_eq!(sink.len(), 2);
    }

    #[tokio::test]
    async fn declared_byte_cap_is_enforced() {
        let sink = MemoryArtifactSink::new().with_max_bytes(4);
        let err = sink.put(&cx(), png(b"12345")).await.unwrap_err();
        assert_eq!(err.kind(), "validation");
        assert!(sink.is_empty(), "an over-cap put must store nothing");
        sink.put(&cx(), png(b"1234")).await.unwrap();
    }

    #[tokio::test]
    async fn blob_sink_round_trips_through_a_blob_store() {
        let blobs = MemoryBlobStore::new();
        let sink = BlobArtifactSink::new(Arc::new(blobs.clone()));
        let reference = sink.put(&cx(), png(b"video bytes")).await.unwrap();
        assert_eq!(
            blobs.get(&reference.sha256).await.unwrap().as_deref(),
            Some(&b"video bytes"[..])
        );
    }

    #[tokio::test]
    async fn conformance_passes_for_shipped_sinks() {
        verify_artifact_sink(&MemoryArtifactSink::new()).await.unwrap();
        verify_artifact_sink(&MemoryArtifactSink::new().with_max_bytes(1024))
            .await
            .unwrap();
        verify_artifact_sink(&BlobArtifactSink::new(Arc::new(MemoryBlobStore::new())))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn conformance_rejects_a_digest_dishonest_sink() {
        struct LyingSink;
        impl ArtifactSink for LyingSink {
            fn put<'a>(
                &'a self,
                _cx: &'a ArtifactCx,
                artifact: NewArtifact,
            ) -> ArtifactFuture<'a, ArtifactRef> {
                Box::pin(async move {
                    Ok(ArtifactRef {
                        id: "art_1".to_string(),
                        uri: None,
                        media_type: artifact.media_type,
                        name: artifact.name,
                        sha256: "not-a-real-digest".to_string(),
                        len: artifact.bytes.len() as u64,
                    })
                })
            }
        }
        let err = verify_artifact_sink(&LyingSink).await.unwrap_err();
        assert!(err.to_string().contains("digest honesty"), "{err}");
    }

    #[tokio::test]
    async fn conformance_rejects_an_advisory_cap() {
        struct AdvisoryCapSink(MemoryArtifactSink);
        impl ArtifactSink for AdvisoryCapSink {
            fn capabilities(&self) -> ArtifactSinkCapabilities {
                ArtifactSinkCapabilities {
                    max_bytes: Some(8),
                }
            }
            fn put<'a>(
                &'a self,
                cx: &'a ArtifactCx,
                artifact: NewArtifact,
            ) -> ArtifactFuture<'a, ArtifactRef> {
                // Ignores its own declared cap.
                self.0.put(cx, artifact)
            }
        }
        let err = verify_artifact_sink(&AdvisoryCapSink(MemoryArtifactSink::new()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max_bytes"), "{err}");
    }
}
