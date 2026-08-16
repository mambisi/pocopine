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

use std::sync::atomic::{AtomicU32, Ordering};

use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, AgentThreadId, ArtifactRef, MAX_APPEND_CHUNK_BYTES,
    MAX_PREVIEW_CHUNK_BYTES, MediaGroupRef, WireArtifactOrigin, WireMediaMode,
};
use pocopine_auth::Principal;

use super::session::BlobStore;

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
    pub thread: Option<AgentThreadId>,
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
    fn put<'a>(
        &'a self,
        cx: &'a ArtifactCx,
        artifact: NewArtifact,
    ) -> ArtifactFuture<'a, ArtifactRef>;
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

/// How a stream's chunks relate to each other and to the final bytes
/// (RFC-122 §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaStreamMode {
    /// Each chunk is a COMPLETE low-fidelity encoding that REPLACES the
    /// previous one (progressive-fidelity image partials). Chunks are never
    /// the authoritative bytes; [`MediaStream::finish_with`] is required.
    Preview,
    /// Chunks CONCATENATE into the byte stream in order (audio/video live
    /// view). [`MediaStream::finish`] may use the concatenation as the final
    /// bytes.
    Append,
}

impl MediaStreamMode {
    fn wire(self) -> WireMediaMode {
        match self {
            Self::Preview => WireMediaMode::Preview,
            Self::Append => WireMediaMode::Append,
        }
    }
}

/// What to open a streamed capture for (RFC-122 §2).
#[derive(Clone, Debug)]
pub struct MediaStreamSpec {
    /// IANA media type of the bytes being produced.
    pub media_type: String,
    /// Optional display name.
    pub name: Option<String>,
    /// How chunks compose.
    pub mode: MediaStreamMode,
    /// Lineage, as on [`NewArtifact`] (RFC-122 §2.2).
    pub derived_from: Vec<String>,
}

/// Where capture-surface events go: the conversational runtime forwards them
/// onto its `AgentEvent` firehose; the typed run is a no-op today (the flow
/// wire has no artifact events yet).
pub(crate) trait ArtifactEvents: Send + Sync {
    fn media_started(
        &self,
        stream_id: &str,
        media_type: &str,
        name: Option<&str>,
        mode: WireMediaMode,
        group: Option<&MediaGroupRef>,
        origin: &WireArtifactOrigin,
    );
    fn media_chunk(&self, stream_id: &str, seq: u32, data_base64: &str);
    fn artifact_produced(
        &self,
        stream_id: Option<&str>,
        artifact: &ArtifactRef,
        group: Option<&MediaGroupRef>,
        derived_from: &[String],
        origin: &WireArtifactOrigin,
    );
}

/// The no-op event surface (typed runs, tests).
pub(crate) struct NoopArtifactEvents;

impl ArtifactEvents for NoopArtifactEvents {
    fn media_started(
        &self,
        _stream_id: &str,
        _media_type: &str,
        _name: Option<&str>,
        _mode: WireMediaMode,
        _group: Option<&MediaGroupRef>,
        _origin: &WireArtifactOrigin,
    ) {
    }
    fn media_chunk(&self, _stream_id: &str, _seq: u32, _data_base64: &str) {}
    fn artifact_produced(
        &self,
        _stream_id: Option<&str>,
        _artifact: &ArtifactRef,
        _group: Option<&MediaGroupRef>,
        _derived_from: &[String],
        _origin: &WireArtifactOrigin,
    ) {
    }
}

/// The host-wired half a dispatcher needs to hand tools a capture surface:
/// built once per turn from the runtime's configured sink + event channel.
pub(crate) struct ArtifactDispatch {
    pub(crate) sink: Arc<dyn ArtifactSink>,
    pub(crate) events: Arc<dyn ArtifactEvents>,
    pub(crate) thread: Option<AgentThreadId>,
    pub(crate) append_budget: usize,
}

/// The sink + append-budget pair stored on the runtime when the host wires
/// `.artifact_sink(...)`.
#[derive(Clone)]
pub(crate) struct ArtifactRuntime {
    pub(crate) sink: Arc<dyn ArtifactSink>,
    pub(crate) append_budget: usize,
}

struct ArtifactsInner {
    sink: Arc<dyn ArtifactSink>,
    events: Arc<dyn ArtifactEvents>,
    principal: Principal,
    thread: Option<AgentThreadId>,
    call_id: String,
    tool_id: String,
    /// Dense output slot within the invocation (RFC-122 §1): one per capture
    /// (a `put` or an opened stream), minted in production order.
    ordinal: AtomicU32,
    /// Group ids minted within the invocation.
    group_seq: AtomicU32,
    append_budget: usize,
}

impl ArtifactsInner {
    fn origin(&self, output_ordinal: u32) -> ArtifactOrigin {
        ArtifactOrigin::Tool {
            call_id: self.call_id.clone(),
            tool_id: self.tool_id.clone(),
            output_ordinal,
        }
    }

    fn wire_origin(&self) -> WireArtifactOrigin {
        WireArtifactOrigin::Tool {
            id: self.call_id.clone(),
            tool: self.tool_id.clone(),
        }
    }
}

/// The artifact capture surface handed to tool code (RFC-122 §2), reached via
/// `ctx.artifacts()`. Scoped to one tool invocation: every capture carries
/// the call's provenance, and every event it emits reaches the turn's
/// firehose without the tool touching the event surface.
#[derive(Clone)]
pub struct Artifacts {
    inner: Arc<ArtifactsInner>,
}

impl Artifacts {
    /// Build the per-invocation surface (dispatcher-only).
    pub(crate) fn for_tool_call(
        dispatch: &ArtifactDispatch,
        principal: Principal,
        call_id: String,
        tool_id: String,
    ) -> Self {
        Self {
            inner: Arc::new(ArtifactsInner {
                sink: dispatch.sink.clone(),
                events: dispatch.events.clone(),
                principal,
                thread: dispatch.thread.clone(),
                call_id,
                tool_id,
                ordinal: AtomicU32::new(0),
                group_seq: AtomicU32::new(0),
                append_budget: dispatch.append_budget,
            }),
        }
    }

    /// Capture `artifact` through the implementor's sink and emit
    /// `ArtifactProduced`. Returns the minted reference — embed it (or its
    /// `uri`) in the tool's ordinary JSON output.
    pub async fn put(&self, artifact: NewArtifact) -> AgenkitResult<ArtifactRef> {
        self.put_with(artifact, None, None).await
    }

    /// Open a streamed capture: live chunks out, one final artifact in.
    /// Emits `MediaStarted` immediately and drives the rest of the RFC-122 §3
    /// stream without the tool ever touching the event surface.
    pub fn stream(&self, spec: MediaStreamSpec) -> MediaStream {
        self.stream_with(spec, None)
    }

    /// Open a multi-output group (n-variant sampling, storyboard batches):
    /// captures opened through it share a minted group id and auto-increment
    /// a dense index. `expected` is the declared output count when known up
    /// front — intent, not promise (RFC-122 §5.3).
    pub fn group(&self, expected: Option<u32>) -> ArtifactGroup {
        let n = self.inner.group_seq.fetch_add(1, Ordering::Relaxed);
        ArtifactGroup {
            artifacts: self.clone(),
            id: format!("{}.g{n}", self.inner.call_id),
            expected,
            next_index: AtomicU32::new(0),
        }
    }

    async fn put_with(
        &self,
        artifact: NewArtifact,
        stream_id: Option<&str>,
        group: Option<&MediaGroupRef>,
    ) -> AgenkitResult<ArtifactRef> {
        let ordinal = self.inner.ordinal.fetch_add(1, Ordering::Relaxed);
        self.capture(artifact, ordinal, stream_id, group).await
    }

    /// Sink the bytes under an already-minted ordinal and emit the terminal
    /// event (shared by direct puts and finishing streams).
    async fn capture(
        &self,
        artifact: NewArtifact,
        ordinal: u32,
        stream_id: Option<&str>,
        group: Option<&MediaGroupRef>,
    ) -> AgenkitResult<ArtifactRef> {
        let cx = ArtifactCx {
            principal: self.inner.principal.clone(),
            thread: self.inner.thread.clone(),
            origin: self.inner.origin(ordinal),
        };
        let derived_from = artifact.derived_from.clone();
        let reference = self.inner.sink.put(&cx, artifact).await?;
        self.inner.events.artifact_produced(
            stream_id,
            &reference,
            group,
            &derived_from,
            &self.inner.wire_origin(),
        );
        Ok(reference)
    }

    fn stream_with(&self, spec: MediaStreamSpec, group: Option<MediaGroupRef>) -> MediaStream {
        let ordinal = self.inner.ordinal.fetch_add(1, Ordering::Relaxed);
        let stream_id = format!("{}.m{ordinal}", self.inner.call_id);
        self.inner.events.media_started(
            &stream_id,
            &spec.media_type,
            spec.name.as_deref(),
            spec.mode.wire(),
            group.as_ref(),
            &self.inner.wire_origin(),
        );
        MediaStream {
            artifacts: self.clone(),
            stream_id,
            ordinal,
            spec,
            group,
            seq: 0,
            appended: Vec::new(),
            wire_sent: 0,
        }
    }
}

/// A multi-output group (RFC-122 §5.3): captures share one group id with
/// dense indexes, so a consumer can slot the variants of one generation.
pub struct ArtifactGroup {
    artifacts: Artifacts,
    id: String,
    expected: Option<u32>,
    next_index: AtomicU32,
}

impl ArtifactGroup {
    fn next_ref(&self) -> MediaGroupRef {
        MediaGroupRef {
            id: self.id.clone(),
            index: self.next_index.fetch_add(1, Ordering::Relaxed),
            expected: self.expected,
        }
    }

    /// Capture one grouped output (see [`Artifacts::put`]).
    pub async fn put(&self, artifact: NewArtifact) -> AgenkitResult<ArtifactRef> {
        let group = self.next_ref();
        self.artifacts.put_with(artifact, None, Some(&group)).await
    }

    /// Open one grouped streamed capture (see [`Artifacts::stream`]).
    pub fn stream(&self, spec: MediaStreamSpec) -> MediaStream {
        let group = self.next_ref();
        self.artifacts.stream_with(spec, Some(group))
    }
}

/// One streamed capture (RFC-122 §5): live chunks out under the declared
/// mode, one final artifact in. Chunks are ephemeral — the artifact is the
/// source of truth. Dropping the stream without finishing is an abort: no
/// `ArtifactProduced` is emitted and consumers discard its chunks.
pub struct MediaStream {
    artifacts: Artifacts,
    stream_id: String,
    /// The output slot minted at open, so interleaved streams keep stable
    /// production order.
    ordinal: u32,
    spec: MediaStreamSpec,
    group: Option<MediaGroupRef>,
    seq: u32,
    /// `Append` mode: the authoritative concatenation (kept even when a
    /// chunk is dropped from the wire — the live view is a courtesy).
    appended: Vec<u8>,
    /// Base64 bytes emitted on the wire so far (`Append` budget accounting).
    wire_sent: usize,
}

impl MediaStream {
    /// This stream's wire correlation id.
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Emit one live chunk under the stream's declared mode. Ephemeral; a
    /// chunk over the mode's byte bound — or past the `Append` stream budget
    /// — is dropped with a warning, never truncated (RFC-122 §5.2). In
    /// `Append` mode the bytes still count toward [`finish`](Self::finish)
    /// regardless: wire drops never corrupt the artifact.
    pub fn chunk(&mut self, data: &[u8]) {
        let encoded = pocopine_codec::base64_encode(data);
        let within = match self.spec.mode {
            MediaStreamMode::Preview => encoded.len() <= MAX_PREVIEW_CHUNK_BYTES,
            MediaStreamMode::Append => {
                self.appended.extend_from_slice(data);
                encoded.len() <= MAX_APPEND_CHUNK_BYTES
                    && self.wire_sent + encoded.len() <= self.artifacts.inner.append_budget
            }
        };
        if !within {
            tracing::warn!(
                target: "pocopine.log",
                stream_id = %self.stream_id,
                chunk_bytes = encoded.len(),
                mode = ?self.spec.mode,
                "media chunk dropped from the wire (over its byte bound); \
                 the final artifact is unaffected"
            );
            return;
        }
        self.artifacts
            .inner
            .events
            .media_chunk(&self.stream_id, self.seq, &encoded);
        self.seq += 1;
        self.wire_sent += encoded.len();
    }

    /// Capture explicit authoritative bytes through the sink and emit
    /// `ArtifactProduced`. Valid in both modes; required in `Preview`
    /// (previews are never the artifact).
    pub async fn finish_with(self, bytes: Vec<u8>) -> AgenkitResult<ArtifactRef> {
        self.capture(bytes).await
    }

    /// `Append` mode only: capture the concatenation of the appended chunks
    /// as the final bytes. Errors in `Preview` mode.
    pub async fn finish(mut self) -> AgenkitResult<ArtifactRef> {
        if self.spec.mode != MediaStreamMode::Append {
            return Err(AgenkitError::validation(format!(
                "media stream `{}` is Preview-mode: previews are never the \
                 authoritative bytes — pass them to finish_with",
                self.stream_id
            )));
        }
        let bytes = std::mem::take(&mut self.appended);
        self.capture(bytes).await
    }

    /// Abandon the stream: no artifact event is emitted; consumers discard
    /// its chunks when the producing call completes without a matching
    /// `ArtifactProduced`.
    pub fn abort(self) {}

    async fn capture(self, bytes: Vec<u8>) -> AgenkitResult<ArtifactRef> {
        let artifact = NewArtifact {
            media_type: self.spec.media_type.clone(),
            name: self.spec.name.clone(),
            bytes,
            derived_from: self.spec.derived_from.clone(),
        };
        self.artifacts
            .capture(
                artifact,
                self.ordinal,
                Some(&self.stream_id),
                self.group.as_ref(),
            )
            .await
    }
}

/// The RFC-122 §4.1 capture rule: route inline media in a model response
/// through the sink — before anything persists or streams — replacing each
/// part with its captured url-form ref. Url-only response media is already a
/// ref and passes through untouched. Inline media with **no sink** is a hard
/// error: generated bytes are never silently dropped (work-or-loud).
pub(crate) async fn capture_response_media(
    content: &mut pocopine_agenkit_core::Content,
    dispatch: Option<&ArtifactDispatch>,
    principal: &Principal,
) -> AgenkitResult<()> {
    use pocopine_agenkit_core::{ContentPart, MediaPart};

    for part in &mut content.parts {
        let ContentPart::Media(media) = part else {
            continue;
        };
        let Some(data) = &media.data_base64 else {
            continue;
        };
        let Some(dispatch) = dispatch else {
            return Err(AgenkitError::config(format!(
                "model returned inline `{}` output but no artifact sink is \
                 configured; wire one with Agenkit::builder().artifact_sink(...)",
                media.media_type
            )));
        };
        let bytes = pocopine_codec::base64_decode(data)
            .map_err(|e| AgenkitError::provider(format!("model media payload: {e}")))?;
        let cx = ArtifactCx {
            principal: principal.clone(),
            thread: dispatch.thread.clone(),
            origin: ArtifactOrigin::Model,
        };
        let artifact = NewArtifact {
            media_type: media.media_type.clone(),
            name: media.name.clone(),
            bytes,
            derived_from: Vec::new(),
        };
        let reference = dispatch.sink.put(&cx, artifact).await?;
        dispatch
            .events
            .artifact_produced(None, &reference, None, &[], &WireArtifactOrigin::Model);
        // The persisted form: bytes live in the sink, the message carries the
        // ref. Replay maps this to a text placeholder at wire build.
        *part = ContentPart::Media(MediaPart {
            media_type: reference.media_type.clone(),
            url: Some(
                reference
                    .uri
                    .clone()
                    .unwrap_or_else(|| format!("artifact:{}", reference.id)),
            ),
            data_base64: None,
            name: reference.name.clone(),
        });
    }
    Ok(())
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
    let again = sink
        .put(&cx, artifact)
        .await
        .map_err(|e| AgenkitError::validation(format!("sink rejects an identical re-put: {e}")))?;
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
            thread: Some(AgentThreadId::new("th_1")),
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
        verify_artifact_sink(&MemoryArtifactSink::new())
            .await
            .unwrap();
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

    #[derive(Default)]
    struct RecordingEvents {
        log: Mutex<Vec<String>>,
    }

    impl ArtifactEvents for RecordingEvents {
        fn media_started(
            &self,
            stream_id: &str,
            media_type: &str,
            _name: Option<&str>,
            mode: WireMediaMode,
            group: Option<&MediaGroupRef>,
            _origin: &WireArtifactOrigin,
        ) {
            self.log.lock().unwrap().push(format!(
                "started {stream_id} {media_type} {mode:?} group={:?}",
                group.map(|g| (g.id.clone(), g.index, g.expected))
            ));
        }
        fn media_chunk(&self, stream_id: &str, seq: u32, data_base64: &str) {
            self.log
                .lock()
                .unwrap()
                .push(format!("chunk {stream_id} {seq} {}b", data_base64.len()));
        }
        fn artifact_produced(
            &self,
            stream_id: Option<&str>,
            artifact: &ArtifactRef,
            group: Option<&MediaGroupRef>,
            derived_from: &[String],
            _origin: &WireArtifactOrigin,
        ) {
            self.log.lock().unwrap().push(format!(
                "produced {:?} {} group={:?} derived={derived_from:?}",
                stream_id,
                artifact.media_type,
                group.map(|g| g.index)
            ));
        }
    }

    fn surface(sink: Arc<dyn ArtifactSink>, budget: usize) -> (Artifacts, Arc<RecordingEvents>) {
        let events = Arc::new(RecordingEvents::default());
        let dispatch = ArtifactDispatch {
            sink,
            events: events.clone(),
            thread: Some(AgentThreadId::new("th_1")),
            append_budget: budget,
        };
        let artifacts = Artifacts::for_tool_call(
            &dispatch,
            Principal::anonymous(),
            "call_1".to_string(),
            "image.generate".to_string(),
        );
        (artifacts, events)
    }

    #[tokio::test]
    async fn put_captures_and_emits_the_terminal_event() {
        let sink = MemoryArtifactSink::new();
        let (artifacts, events) = surface(Arc::new(sink.clone()), 1024);
        let reference = artifacts.put(png(b"pixels")).await.unwrap();
        assert_eq!(sink.bytes(&reference.id).as_deref(), Some(&b"pixels"[..]));
        let log = events.log.lock().unwrap().clone();
        assert_eq!(log, vec!["produced None image/png group=None derived=[]"]);
    }

    #[tokio::test]
    async fn preview_stream_emits_started_chunks_and_produced_in_order() {
        let (artifacts, events) = surface(Arc::new(MemoryArtifactSink::new()), 1024);
        let mut stream = artifacts.stream(MediaStreamSpec {
            media_type: "image/png".to_string(),
            name: Some("out.png".to_string()),
            mode: MediaStreamMode::Preview,
            derived_from: vec!["art_0".to_string()],
        });
        stream.chunk(b"lofi");
        stream.chunk(b"hifi");
        let reference = stream.finish_with(b"final pixels".to_vec()).await.unwrap();
        assert_eq!(reference.len, 12);

        let log = events.log.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "started call_1.m0 image/png Preview group=None".to_string(),
                "chunk call_1.m0 0 8b".to_string(),
                "chunk call_1.m0 1 8b".to_string(),
                "produced Some(\"call_1.m0\") image/png group=None derived=[\"art_0\"]".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn preview_finish_without_bytes_is_a_loud_error() {
        let (artifacts, _) = surface(Arc::new(MemoryArtifactSink::new()), 1024);
        let stream = artifacts.stream(MediaStreamSpec {
            media_type: "image/png".to_string(),
            name: None,
            mode: MediaStreamMode::Preview,
            derived_from: Vec::new(),
        });
        let err = stream.finish().await.unwrap_err();
        assert!(err.to_string().contains("finish_with"), "{err}");
    }

    #[tokio::test]
    async fn append_stream_concatenates_and_survives_wire_drops() {
        // Budget of 12 base64 bytes: the first chunk (8) fits, the second
        // (8) would exceed it and is dropped from the wire — but finish()
        // still captures BOTH chunks (the wire is a courtesy view).
        let sink = MemoryArtifactSink::new();
        let (artifacts, events) = surface(Arc::new(sink.clone()), 12);
        let mut stream = artifacts.stream(MediaStreamSpec {
            media_type: "video/mp4".to_string(),
            name: None,
            mode: MediaStreamMode::Append,
            derived_from: Vec::new(),
        });
        stream.chunk(b"aaaaaa");
        stream.chunk(b"bbbbbb");
        let reference = stream.finish().await.unwrap();
        assert_eq!(
            sink.bytes(&reference.id).as_deref(),
            Some(&b"aaaaaabbbbbb"[..])
        );
        let log = events.log.lock().unwrap().clone();
        assert_eq!(log.len(), 3, "started + ONE chunk + produced: {log:?}");
        assert!(log[1].starts_with("chunk"), "{log:?}");
    }

    #[tokio::test]
    async fn aborted_stream_emits_no_artifact() {
        let (artifacts, events) = surface(Arc::new(MemoryArtifactSink::new()), 1024);
        let mut stream = artifacts.stream(MediaStreamSpec {
            media_type: "image/png".to_string(),
            name: None,
            mode: MediaStreamMode::Preview,
            derived_from: Vec::new(),
        });
        stream.chunk(b"partial");
        stream.abort();
        let log = events.log.lock().unwrap().clone();
        assert!(
            !log.iter().any(|line| line.starts_with("produced")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn groups_share_an_id_and_index_densely() {
        let (artifacts, events) = surface(Arc::new(MemoryArtifactSink::new()), 1024);
        let group = artifacts.group(Some(2));
        group.put(png(b"variant a")).await.unwrap();
        group.put(png(b"variant b")).await.unwrap();
        // A second group mints a distinct id; an ungrouped put carries none.
        let other = artifacts.group(None);
        other.put(png(b"solo")).await.unwrap();
        artifacts.put(png(b"free")).await.unwrap();

        let log = events.log.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "produced None image/png group=Some(0) derived=[]".to_string(),
                "produced None image/png group=Some(1) derived=[]".to_string(),
                "produced None image/png group=Some(0) derived=[]".to_string(),
                "produced None image/png group=None derived=[]".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn conformance_rejects_an_advisory_cap() {
        struct AdvisoryCapSink(MemoryArtifactSink);
        impl ArtifactSink for AdvisoryCapSink {
            fn capabilities(&self) -> ArtifactSinkCapabilities {
                ArtifactSinkCapabilities { max_bytes: Some(8) }
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
