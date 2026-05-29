use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{SyncError, SyncResult};

/// Type alias for the wire-level stream parameter map used by shape
/// subscriptions. Sorted by key so the canonical JSON encoding (and
/// derived cache hash) are deterministic across builds and clients.
pub type StreamParams = BTreeMap<String, Value>;

/// Current sync protocol identifier.
pub const SYNC_PROTOCOL_V1: &str = "pocopine.sync.v1";
/// Maximum length, in bytes, accepted for protocol token strings.
pub const MAX_SYNC_TOKEN_LEN: usize = 1024;

macro_rules! sync_path {
    ($suffix:literal) => {
        concat!("/__pocopine/sync/v1", $suffix)
    };
}

/// Default sync endpoint prefix mounted by the server plugin.
pub const SYNC_ENDPOINT_PREFIX: &str = sync_path!("");
/// Open endpoint path.
pub const SYNC_OPEN_PATH: &str = sync_path!("/open");
/// Pull endpoint path.
pub const SYNC_PULL_PATH: &str = sync_path!("/pull");
/// Push endpoint path.
pub const SYNC_PUSH_PATH: &str = sync_path!("/push");

fn validate_token(field: &'static str, value: String) -> SyncResult<String> {
    let trimmed = value.trim();
    if value.len() > MAX_SYNC_TOKEN_LEN
        || trimmed.is_empty()
        || trimmed != value
        || value.chars().any(char::is_control)
    {
        return Err(SyncError::invalid_value(field, value));
    }
    // Stream-name-only: reject `:` because the live-wakeup wire
    // protocol uses it as a structural delimiter (see
    // [`sync_stream_tag`] / [`sync_stream_params_tag`] / RFC 088
    // §C). A stream named `"issues:abcd1234abcd1234"` would
    // produce the bare topic `query:sync:stream:issues:abcd…`
    // which is indistinguishable from a per-params topic for the
    // public `issues` prefix — a public-prefix allowlist would
    // then authorize access to the private sibling stream. We
    // forbid the collision at the source of truth (the token
    // validator) rather than chase it downstream.
    //
    // Field-specific via the `field` discriminator — RowKey,
    // MutationId, etc. legitimately use `:` in composite keys.
    // Only the stream token namespace is protocol-reserved.
    if field == "stream" && value.contains(':') {
        return Err(SyncError::invalid_value(field, value));
    }
    Ok(value)
}

macro_rules! opaque_string_type {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(String);

        impl $name {
            /// Build a validated value.
            pub fn new(value: impl Into<String>) -> SyncResult<Self> {
                validate_token($field, value.into()).map(Self)
            }

            /// Borrow the string value.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::str::FromStr for $name {
            type Err = SyncError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = SyncError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = SyncError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

opaque_string_type!(
    SyncStreamName,
    "stream",
    "Server-registered sync stream name."
);
opaque_string_type!(
    SyncCollectionName,
    "collection",
    "Public collection name exposed to the client."
);
opaque_string_type!(SyncCursor, "cursor", "Opaque server-issued sync cursor.");
opaque_string_type!(
    RowKey,
    "row key",
    "Public row identity inside one sync stream."
);
opaque_string_type!(
    RowVersion,
    "row version",
    "Opaque server-issued row version."
);
opaque_string_type!(
    MutationId,
    "mutation id",
    "Client-generated mutation idempotency key."
);

impl MutationId {
    /// Generate a UUIDv7-backed mutation id. The default constructor
    /// used by `TypedMutation::push(&qc)` and any other framework
    /// auto-id path.
    ///
    /// UUIDv7 is time-ordered (the leading 48 bits are an ms-resolution
    /// Unix timestamp), so mutation logs and offline replay queues keep
    /// their insertion order without an extra sort key. The string form
    /// (`xxxxxxxx-xxxx-7xxx-yxxx-xxxxxxxxxxxx`) is always 36 bytes —
    /// well under `MutationId`'s opaque-string length cap — so the
    /// `unwrap` below is infallible.
    pub fn uuid() -> Self {
        Self::new(uuid::Uuid::now_v7().to_string()).expect("UUIDv7 is always a valid MutationId")
    }
}

opaque_string_type!(
    SyncDeviceId,
    "device id",
    "Stable client device identity persisted by a local sync store."
);
opaque_string_type!(
    SyncSessionId,
    "session id",
    "Ephemeral sync session identity for one running client instance."
);

/// Live query tag used to wake clients for a sync stream.
pub fn sync_stream_tag(stream: &str) -> String {
    format!("sync:stream:{stream}")
}

/// Number of lowercase-hex digits in the per-params topic suffix
/// (RFC 088 §C). The suffix is `{params_hash:016x}` — a `u64`
/// formatted as 16 lowercase hex digits — so the validator in
/// `pocopine_live::is_params_hash_suffix` checks against
/// `PARAMS_HASH_HEX_LEN + 1` total bytes (the leading `:` plus 16
/// hex digits). Exporting the constant keeps the formatter and the
/// validator coupled so a future widening of the hash (say, to 128
/// bits / 32 hex chars) updates both call sites simultaneously.
pub const PARAMS_HASH_HEX_LEN: usize = 16;

/// Per-(stream, params) live query tag, used for RFC 088 §C precise
/// invalidation routing. Same prefix as [`sync_stream_tag`] plus a
/// `PARAMS_HASH_HEX_LEN`-hex suffix derived from
/// [`stream_params_hash`].
///
/// Wire shape: `sync:stream:{stream}:{params_hash:016x}`.
///
/// Mirrors the bare-tag convention; clients on the new protocol
/// subscribe to BOTH the bare and per-params topics during rollout
/// (see RFC 088 §C.6). Servers publish to per-params iff the source
/// returns non-empty params from
/// [`SyncStreamSource::row_to_params`](crate::server::SyncStreamSource::row_to_params).
pub fn sync_stream_params_tag(stream: &str, params_hash: u64) -> String {
    format!("sync:stream:{stream}:{params_hash:016x}")
}

/// FNV-1a 64-bit hash over `(stream_name, sorted-by-key params JSON)`.
/// Closely related to [`local_stream_key`] — both use the same
/// FNV-1a constants and the same `stream + 0x00 + canonical_json`
/// byte recipe — but with one edge-case difference: `local_stream_key`
/// short-circuits to the bare stream name for `params.is_empty()`,
/// while this function always hashes (folding in the separator byte
/// plus the empty-object JSON `"{}"`). In practice the wire-side
/// path only calls this for non-empty params (the publish helper
/// guards on empty), so the divergence is invisible; cross-language
/// porters matching this byte spec for cache scoping should mirror
/// the always-hash form here rather than copying
/// `local_stream_key`'s short-circuit.
///
/// This is the cross-language contract for RFC 088 §C — the
/// SERVER computes this from the row's `row_to_params` projection,
/// the CLIENT computes it from the captured subscription params,
/// and the two MUST agree byte-for-byte. The hash function is FNV-1a
/// (not cryptographic) and `StreamParams = BTreeMap<String, Value>`
/// serializes in sorted-key order by construction, so the only
/// portability constraints are: same FNV-1a constants, same JSON
/// encoding (no insignificant whitespace, no number/float coercion).
///
/// The golden-fixture test at
/// `tests/stream_params_hash_fixture.rs` pins a set of (stream,
/// params) → hex-hash pairs that future ports (Swift / Go / Kotlin
/// clients) MUST reproduce.
pub fn stream_params_hash(stream: &str, params: &StreamParams) -> u64 {
    // Canonical JSON of a `BTreeMap<String, Value>` is infallible
    // for any value that `serde_json::Value` can hold (Value rejects
    // NaN/Infinity at construction time). The only documented
    // failure path is `serde_json`'s default 128-level recursion
    // limit on deeply-nested `Value::Object`/`Value::Array`. A
    // silent `unwrap_or_default()` here would collapse every such
    // failure to the same hash (FNV-1a of `stream + 0x00`), aliasing
    // distinct subscriptions onto one topic — codex caught the
    // regression in review. We `expect()` instead so the failure
    // surfaces as a clear panic at the publish site rather than as
    // mis-routed live wakeups in production.
    let canonical = serde_json::to_vec(params)
        .expect("BTreeMap<String, Value> canonical JSON is infallible for serde_json::Value");
    let mut buf: Vec<u8> = Vec::with_capacity(stream.len() + 1 + canonical.len());
    buf.extend_from_slice(stream.as_bytes());
    // Separator so `stream="ab", params={}` ≠ `stream="a", params={b:…}`.
    buf.push(0u8);
    buf.extend_from_slice(&canonical);
    fnv1a_64(&buf)
}

/// Stable client-side cache key that combines a sync stream name with
/// a hash of its subscription params. Used to scope the local store so
/// two subscriptions to the same `stream` with different `params` do
/// NOT share cached rows, cursor, or pending mutations.
///
/// **Wire-vs-local split:** every `/open`, `/pull`, and `/push`
/// envelope carries the BARE `stream` plus a `params` map. The local
/// store, by contrast, sees a composite name so its existing
/// stream-keyed API works without a breaking change.
///
/// **Backwards-compat:** when `params` is empty, the returned key is
/// the bare stream name (identical to pre-RFC-085 on-disk data). Old
/// snapshots in any `SyncLocalStore` impl keep hydrating cleanly for
/// unparameterized subscriptions.
///
/// **Hash:** FNV-1a 64-bit over the canonical JSON encoding of the
/// `BTreeMap<String, Value>` (sorted by key by construction). This is
/// NOT a cryptographic hash — it's purely for cache disambiguation
/// inside one client; collisions would shred a single client's cache
/// but never leak data cross-client.
pub fn local_stream_key(stream: &SyncStreamName, params: &StreamParams) -> SyncStreamName {
    if params.is_empty() {
        return stream.clone();
    }
    let stream_str = stream.as_str();
    // Hash the FULL stream name plus a separator plus the canonical
    // params JSON. Two long stream names that share a truncated
    // prefix but differ in their suffix would otherwise produce
    // identical composite keys after the prefix truncation below.
    // Mixing the full stream into the hash gives every (stream, params)
    // pair a unique 64-bit suffix even if the visible prefix collides.
    let canonical = serde_json::to_vec(params).unwrap_or_default();
    let mut hasher_input: Vec<u8> = Vec::with_capacity(stream_str.len() + 1 + canonical.len());
    hasher_input.extend_from_slice(stream_str.as_bytes());
    hasher_input.push(0u8); // separator so `stream_str=ab + p={}` ≠ `stream_str=a + p=b{}`.
    hasher_input.extend_from_slice(&canonical);
    let hash = fnv1a_64(&hasher_input);
    let suffix = format!("__params_{:016x}", hash);
    // Cap the composite at MAX_SYNC_TOKEN_LEN by truncating the stream
    // prefix at a char boundary. Even when the prefix is truncated, the
    // suffix carries the full-stream + params hash, so distinct streams
    // with the same params still get distinct composite keys.
    let max_stream_len = MAX_SYNC_TOKEN_LEN.saturating_sub(suffix.len());
    let stream_part = if stream_str.len() <= max_stream_len {
        stream_str
    } else {
        let mut end = max_stream_len;
        while end > 0 && !stream_str.is_char_boundary(end) {
            end -= 1;
        }
        &stream_str[..end]
    };
    let composite = format!("{stream_part}{suffix}");
    // Truncation preserves byte composition up to a char boundary;
    // the source `stream_str` already passed `validate_token`, so the
    // truncated prefix + ASCII suffix still satisfies the token
    // contract. Any failure here would indicate a framework bug.
    SyncStreamName::new(composite).expect("local_stream_key fits the sync token budget")
}

/// FNV-1a 64-bit. Inline because we don't want a hash-crate dep for
/// the four bytes of work this does.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Operation attached to a sync change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOp {
    Upsert,
    Delete,
    Reset,
}

/// Whether a pull response is a full replacement or incremental batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPullMode {
    Snapshot,
    Incremental,
}

/// Row payload plus sync metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncRow<T> {
    pub key: RowKey,
    pub version: Option<RowVersion>,
    pub value: T,
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub conflict: bool,
}

impl<T> SyncRow<T> {
    pub fn new(key: impl Into<String>, value: T) -> SyncResult<Self> {
        Ok(Self {
            key: RowKey::new(key)?,
            version: None,
            value,
            pending: false,
            conflict: false,
        })
    }

    pub fn version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.version = Some(RowVersion::new(version)?);
        Ok(self)
    }

    /// Attach an already validated row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.version = Some(version);
        self
    }

    /// Mark whether this row is waiting for a local push outcome.
    pub fn pending(mut self, pending: bool) -> Self {
        self.pending = pending;
        self
    }

    /// Mark whether this row is showing a server conflict.
    pub fn conflict(mut self, conflict: bool) -> Self {
        self.conflict = conflict;
        self
    }
}

/// One ordered change in a sync stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncChange<T> {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub row: Option<SyncRow<T>>,
    pub cursor: SyncCursor,
}

/// Subscription to one stream, optionally narrowed by typed params.
///
/// Wire backwards-compat: this struct accepts both
/// `{ "stream": "name", "params": {...} }` AND a bare `"name"` string,
/// so a pre-RFC-085 client whose `SyncOpenRequest.streams` carried
/// `Vec<SyncStreamName>` continues to deserialize cleanly into
/// `SyncStreamSubscription { stream: "name", params: {} }`.
///
/// **Symmetric serialize:** when `params` is empty we emit the bare
/// string form so a new client talking to an old server (rolling
/// deploy without server-first ordering) keeps working — the
/// pre-RFC-085 server's `Vec<SyncStreamName>` deserializer only
/// accepts strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncStreamSubscription {
    pub stream: SyncStreamName,
    pub params: StreamParams,
}

impl SyncStreamSubscription {
    pub fn new(stream: SyncStreamName) -> Self {
        Self {
            stream,
            params: BTreeMap::new(),
        }
    }

    pub fn with_params(mut self, params: StreamParams) -> Self {
        self.params = params;
        self
    }
}

impl From<SyncStreamName> for SyncStreamSubscription {
    fn from(stream: SyncStreamName) -> Self {
        Self::new(stream)
    }
}

impl Serialize for SyncStreamSubscription {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.params.is_empty() {
            // Bare-string form. Symmetric with the pre-RFC-085 wire
            // shape, so a new client serializing an unparameterized
            // subscription remains readable by an old server that
            // expects `Vec<SyncStreamName>`.
            self.stream.serialize(serializer)
        } else {
            use serde::ser::SerializeStruct;
            let mut state = serializer.serialize_struct("SyncStreamSubscription", 2)?;
            state.serialize_field("stream", &self.stream)?;
            state.serialize_field("params", &self.params)?;
            state.end()
        }
    }
}

impl<'de> Deserialize<'de> for SyncStreamSubscription {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{Error as DeError, MapAccess, Visitor};
        use std::fmt;

        struct SubVisitor;

        impl<'de> Visitor<'de> for SubVisitor {
            type Value = SyncStreamSubscription;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a stream name string OR an object \
                     `{ \"stream\": <name>, \"params\": {...} }`",
                )
            }

            // Accept the legacy bare-string form.
            fn visit_str<E: DeError>(self, value: &str) -> Result<Self::Value, E> {
                let stream = SyncStreamName::new(value).map_err(DeError::custom)?;
                Ok(SyncStreamSubscription::new(stream))
            }

            fn visit_string<E: DeError>(self, value: String) -> Result<Self::Value, E> {
                let stream = SyncStreamName::new(value).map_err(DeError::custom)?;
                Ok(SyncStreamSubscription::new(stream))
            }

            // Accept the object form with explicit field-name diagnostics
            // (an untagged enum would silently fall back to default-empty
            // params on a typo like "param": instead of "params":).
            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut stream: Option<SyncStreamName> = None;
                let mut params: Option<StreamParams> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "stream" => {
                            if stream.is_some() {
                                return Err(DeError::duplicate_field("stream"));
                            }
                            stream = Some(map.next_value()?);
                        }
                        "params" => {
                            if params.is_some() {
                                return Err(DeError::duplicate_field("params"));
                            }
                            // Treat explicit `null` as empty map so
                            // non-Rust clients (TS / Python) that emit
                            // null for unset fields don't break.
                            params = Some(
                                map.next_value::<Option<StreamParams>>()?
                                    .unwrap_or_default(),
                            );
                        }
                        other => {
                            return Err(DeError::unknown_field(other, &["stream", "params"]));
                        }
                    }
                }
                let stream = stream.ok_or_else(|| DeError::missing_field("stream"))?;
                Ok(SyncStreamSubscription {
                    stream,
                    params: params.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_any(SubVisitor)
    }
}

/// Deserializer for a `StreamParams` field that accepts:
///
/// * the field being absent (handled by `#[serde(default)]`),
/// * the field being explicit JSON `null` (TS / Python clients), AND
/// * a normal object.
///
/// Both `missing` and `null` collapse to an empty map. Without this,
/// an explicit `"params": null` would fail with `invalid type: null,
/// expected a map` which a non-Rust client can't distinguish from a
/// network error.
pub(crate) fn deserialize_params_null_as_default<'de, D>(
    deserializer: D,
) -> Result<StreamParams, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<StreamParams>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

/// Open one or more streams.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenRequest {
    pub protocol: String,
    #[serde(default)]
    pub client_id: Option<SyncDeviceId>,
    pub streams: Vec<SyncStreamSubscription>,
}

impl SyncOpenRequest {
    pub fn new<I, S>(streams: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SyncStreamSubscription>,
    {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            client_id: None,
            streams: streams.into_iter().map(Into::into).collect(),
        }
    }

    pub fn client_id(mut self, client_id: SyncDeviceId) -> Self {
        self.client_id = Some(client_id);
        self
    }
}

/// Stream accepted by an open response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenStream {
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub cursor: Option<SyncCursor>,
    /// Application-level schema version the server is serving for this
    /// stream. Authors declare it via `#[resource(schema_version = N)]`
    /// (or directly on `SyncStreamSource::schema_version`); the client
    /// compares against its cached value to decide whether the local cache
    /// is still valid. The `#[serde(default)]` makes the wire field
    /// backwards-compatible: an old server response without the field
    /// deserializes as `1`, and an old client reading a field-bearing
    /// response just ignores it (serde drops unknown fields). Explicit
    /// JSON `null` is also coerced to the default for clients (TS,
    /// Python) that emit `null` for unset fields.
    #[serde(
        default = "default_schema_version_one",
        deserialize_with = "deserialize_schema_version_default_one"
    )]
    pub schema_version: u32,
    /// Params the server accepted for this subscription, echoed back
    /// so the client can confirm what the server is actually serving.
    /// Empty when the subscription is not parameterized; the field is
    /// `#[serde(default)]` for backwards-compat with pre-RFC-085 servers.
    /// Explicit JSON `null` is also coerced to empty for clients (TS,
    /// Python) that emit `null` for unset fields.
    #[serde(
        default,
        deserialize_with = "deserialize_params_null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub params: StreamParams,
}

/// Default schema version when the wire field is missing or the source
/// doesn't override the trait default. `1` means "first version of the
/// schema" — apps that have never bumped never observe this field.
pub fn default_schema_version_one() -> u32 {
    1
}

/// Custom deserializer for the wire `schema_version` field. Accepts:
///
/// * the field being absent (handled by `#[serde(default = ...)]` —
///   this deserializer isn't called),
/// * the field being explicit `null` (TypeScript / Python clients
///   that emit `null` for unset fields), AND
/// * a normal `u32` value.
///
/// Both `missing` and `null` collapse to
/// [`default_schema_version_one`]. Without this, an explicit
/// `"schema_version": null` would fail deserialization with `invalid
/// type: null, expected u32`, which a non-Rust client cannot
/// distinguish from a network error.
pub(crate) fn deserialize_schema_version_default_one<'de, D>(
    deserializer: D,
) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<u32>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_else(default_schema_version_one))
}

/// Open response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenResponse {
    pub protocol: String,
    pub streams: Vec<SyncOpenStream>,
}

impl SyncOpenResponse {
    pub fn new(streams: Vec<SyncOpenStream>) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            streams,
        }
    }
}

/// Pull request for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncPullRequest {
    pub protocol: String,
    pub stream: SyncStreamName,
    /// Filter params for the subscription, must match what was sent
    /// on `/open` so the server's cursor + filter view stays
    /// consistent. Empty for unparameterized subscriptions;
    /// `#[serde(default)]` keeps old-client compat. Explicit JSON
    /// `null` is coerced to empty for non-Rust clients (TS, Python).
    #[serde(
        default,
        deserialize_with = "deserialize_params_null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub params: StreamParams,
    pub cursor: Option<SyncCursor>,
    pub limit: u32,
}

impl SyncPullRequest {
    pub fn new(stream: SyncStreamName) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            params: BTreeMap::new(),
            cursor: None,
            limit: 500,
        }
    }

    pub fn cursor(mut self, cursor: Option<SyncCursor>) -> Self {
        self.cursor = cursor;
        self
    }

    /// Attach the subscription's filter params. Must match what was
    /// sent on `/open` for the same `(stream, params)` pair.
    pub fn params(mut self, params: StreamParams) -> Self {
        self.params = params;
        self
    }
}

/// Pull response for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncPullResponse<T> {
    pub protocol: String,
    pub stream: SyncStreamName,
    pub collection: SyncCollectionName,
    pub mode: SyncPullMode,
    #[serde(default)]
    pub rows: Vec<SyncRow<T>>,
    #[serde(default)]
    pub changes: Vec<SyncChange<T>>,
    pub cursor: Option<SyncCursor>,
    pub has_more: bool,
}

impl<T> SyncPullResponse<T> {
    pub fn snapshot(
        stream: SyncStreamName,
        collection: SyncCollectionName,
        rows: Vec<SyncRow<T>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            collection,
            mode: SyncPullMode::Snapshot,
            rows,
            changes: Vec::new(),
            cursor,
            has_more: false,
        }
    }

    pub fn incremental(
        stream: SyncStreamName,
        collection: SyncCollectionName,
        changes: Vec<SyncChange<T>>,
        cursor: Option<SyncCursor>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            collection,
            mode: SyncPullMode::Incremental,
            rows: Vec::new(),
            changes,
            cursor,
            has_more: false,
        }
    }
}

/// Client mutation envelope. The first slice defines the wire format;
/// concrete mutation application belongs to stream sources.
///
/// The two server-side sidecars (`migrated_payload`, `migration_error`)
/// are set by the framework's `push_handler` when a stale-schema
/// request runs through `SyncStreamSource::migrate_payload`. They are
/// `#[serde(skip)]`, never on the wire, and always `None` for
/// client-built mutations. Sources consume them via
/// `take_processing_payload` AFTER consulting their idempotency log
/// against the ORIGINAL `payload` — so a retry of an
/// already-accepted mutation succeeds even when the registered
/// migrator now rejects (or panics) on the same inputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct ClientMutation<M> {
    pub id: MutationId,
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
    pub payload: M,
    /// Server-side sidecar: the result of `migrate_payload` on this
    /// mutation. `Some(Ok(value))` is the migrated payload to apply;
    /// `Some(Err(reason))` is a migration failure to surface IF the
    /// mutation isn't already idempotent-accepted; `None` means no
    /// migration was needed (request schema matches server schema).
    #[serde(skip)]
    pub migration_outcome: Option<MigrationOutcome<M>>,
}

/// Outcome of `migrate_payload` for one mutation, carried as a
/// server-only sidecar inside `ClientMutation`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationOutcome<M> {
    /// Successful migration: this is the payload to deserialize and
    /// apply (the original wire payload is preserved in `payload`
    /// for idempotency-log comparison).
    Migrated(M),
    /// Migration failed. The reason should surface as a per-mutation
    /// rejection — but ONLY if the source's idempotency log doesn't
    /// already have this mutation_id+payload as accepted. A retry of
    /// a previously-accepted mutation should always succeed
    /// regardless of whether the current migrator would accept it.
    Failed { reason: String },
}

impl<M> ClientMutation<M> {
    /// Build a mutation with a caller-supplied id.
    pub fn new(id: MutationId, op: SyncOp, payload: M) -> Self {
        Self {
            id,
            key: None,
            op,
            base_version: None,
            payload,
            migration_outcome: None,
        }
    }

    /// Take the payload the source should apply, consuming the
    /// migration sidecar. Returns:
    ///
    /// * `Ok(value)` — the value to deserialize and apply (migrated
    ///   if migration ran successfully, otherwise the original wire
    ///   payload).
    /// * `Err(reason)` — the migrator rejected this mutation. The
    ///   caller must already have checked its idempotency log
    ///   against `mutation.payload` — only mutations NOT in the log
    ///   should surface this error.
    ///
    /// `mutation.payload` is left intact for idempotency comparisons.
    pub fn take_processing_payload(&mut self) -> Result<M, String>
    where
        M: Clone,
    {
        match self.migration_outcome.take() {
            Some(MigrationOutcome::Migrated(value)) => Ok(value),
            Some(MigrationOutcome::Failed { reason }) => Err(reason),
            None => Ok(self.payload.clone()),
        }
    }

    /// Build an upsert mutation with a caller-supplied id.
    pub fn upsert(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Upsert, payload)
    }

    /// Build a delete mutation with a caller-supplied id.
    pub fn delete(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Delete, payload)
    }

    /// Build a reset mutation with a caller-supplied id.
    pub fn reset(id: MutationId, payload: M) -> Self {
        Self::new(id, SyncOp::Reset, payload)
    }

    /// Build a mutation scoped to a row.
    pub fn for_row(
        id: MutationId,
        op: SyncOp,
        key: impl Into<String>,
        payload: M,
    ) -> SyncResult<Self> {
        Self::new(id, op, payload).key(key)
    }

    /// Build an upsert mutation scoped to a row.
    pub fn upsert_row(id: MutationId, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::upsert(id, payload).key(key)
    }

    /// Build a delete mutation scoped to a row.
    pub fn delete_row(id: MutationId, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::delete(id, payload).key(key)
    }

    /// Attach a row key.
    pub fn key(mut self, key: impl Into<String>) -> SyncResult<Self> {
        self.key = Some(RowKey::new(key)?);
        Ok(self)
    }

    /// Attach an already validated row key.
    pub fn row_key(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Attach a base row version for conflict detection.
    pub fn base_version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.base_version = Some(RowVersion::new(version)?);
        Ok(self)
    }

    /// Attach an already validated base row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Attach an optional already validated base row version.
    pub fn base_row_version(mut self, version: Option<RowVersion>) -> Self {
        self.base_version = version;
        self
    }
}

/// Client mutation before the local store reserves a durable mutation id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct ClientMutationDraft<M> {
    pub key: Option<RowKey>,
    pub op: SyncOp,
    pub base_version: Option<RowVersion>,
    pub payload: M,
}

impl<M> ClientMutationDraft<M> {
    /// Build a draft mutation for the given operation.
    pub fn new(op: SyncOp, payload: M) -> Self {
        Self {
            key: None,
            op,
            base_version: None,
            payload,
        }
    }

    /// Build an upsert draft.
    pub fn upsert(payload: M) -> Self {
        Self::new(SyncOp::Upsert, payload)
    }

    /// Build a delete draft.
    pub fn delete(payload: M) -> Self {
        Self::new(SyncOp::Delete, payload)
    }

    /// Build a reset draft.
    pub fn reset(payload: M) -> Self {
        Self::new(SyncOp::Reset, payload)
    }

    /// Build a draft mutation scoped to a row.
    pub fn for_row(op: SyncOp, key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::new(op, payload).key(key)
    }

    /// Build an upsert draft scoped to a row.
    pub fn upsert_row(key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::upsert(payload).key(key)
    }

    /// Build a delete draft scoped to a row.
    pub fn delete_row(key: impl Into<String>, payload: M) -> SyncResult<Self> {
        Self::delete(payload).key(key)
    }

    /// Attach a row key.
    pub fn key(mut self, key: impl Into<String>) -> SyncResult<Self> {
        self.key = Some(RowKey::new(key)?);
        Ok(self)
    }

    /// Attach an already validated row key.
    pub fn row_key(mut self, key: RowKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Attach a base row version for conflict detection.
    pub fn base_version(mut self, version: impl Into<String>) -> SyncResult<Self> {
        self.base_version = Some(RowVersion::new(version)?);
        Ok(self)
    }

    /// Attach an already validated base row version.
    pub fn row_version(mut self, version: RowVersion) -> Self {
        self.base_version = Some(version);
        self
    }

    /// Attach an optional already validated base row version.
    pub fn base_row_version(mut self, version: Option<RowVersion>) -> Self {
        self.base_version = version;
        self
    }

    /// Convert this draft into a wire mutation after an id is reserved.
    pub fn with_id(self, id: MutationId) -> ClientMutation<M> {
        ClientMutation {
            id,
            key: self.key,
            op: self.op,
            base_version: self.base_version,
            payload: self.payload,
            migration_outcome: None,
        }
    }
}

/// Push request for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct SyncPushRequest<M> {
    pub protocol: String,
    pub stream: SyncStreamName,
    /// Filter params for the subscription this push belongs to. Must
    /// match what was sent on `/open` so the server can authorize +
    /// route mutations to the right filtered view. Empty for
    /// unparameterized subscriptions; `#[serde(default)]` keeps
    /// old-client compat. Explicit JSON `null` is coerced to empty
    /// for non-Rust clients (TS, Python).
    #[serde(
        default,
        deserialize_with = "deserialize_params_null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub params: StreamParams,
    #[serde(default)]
    pub mutations: Vec<ClientMutation<M>>,
    /// Application-level schema version the CLIENT encoded these
    /// mutations against. The server compares against
    /// `SyncStreamSource::schema_version()`; when the client is on an
    /// OLDER version, the source's `migrate_payload` is invoked per
    /// mutation. A source that hasn't registered a migrator rejects
    /// each mutation with `SyncError::SchemaMigration`. Defaults to
    /// `1` so an old client that doesn't send the field is treated
    /// as v1; explicit JSON `null` is coerced to the default too,
    /// for clients (TS, Python) that emit `null` for unset fields.
    #[serde(
        default = "default_schema_version_one",
        deserialize_with = "deserialize_schema_version_default_one"
    )]
    pub schema_version: u32,
}

impl<M> SyncPushRequest<M> {
    pub fn new(
        stream: SyncStreamName,
        mutations: impl IntoIterator<Item = ClientMutation<M>>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            params: BTreeMap::new(),
            mutations: mutations.into_iter().collect(),
            schema_version: default_schema_version_one(),
        }
    }

    /// Attach the subscription's filter params. Must match what was
    /// sent on `/open` for the same `(stream, params)` pair.
    pub fn params(mut self, params: StreamParams) -> Self {
        self.params = params;
        self
    }

    /// Set the application-level schema version the mutations are
    /// encoded under. Generated client helpers fill this from the
    /// resource's compile-time `SCHEMA_VERSION` constant. `0` is
    /// coerced to `1` (the framework's canonical default) so the
    /// builder cannot accidentally smuggle an out-of-range value
    /// onto the wire — the server's push handler also rejects `0`
    /// defensively in case a raw `SyncPushRequest` is constructed
    /// via struct literal.
    pub fn with_schema_version(mut self, schema_version: u32) -> Self {
        self.schema_version = if schema_version == 0 {
            default_schema_version_one()
        } else {
            schema_version
        };
        self
    }
}

/// Conflict returned by a push.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncConflict<T> {
    pub mutation_id: MutationId,
    pub key: Option<RowKey>,
    pub server_row: Option<SyncRow<T>>,
    pub reason: String,
}

/// Rejected mutation returned by a push.
///
/// **Ordering:** `SyncPushResponse::rejected` is NOT guaranteed to
/// match the request's mutation order. The server may surface
/// schema-migration rejections after source-side rejections (or
/// vice versa). Consumers should correlate by `mutation_id`, not by
/// index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncRejectedMutation {
    pub mutation_id: MutationId,
    pub key: Option<RowKey>,
    pub reason: String,
}

/// Push response for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SyncPushResponse<T> {
    pub protocol: String,
    pub stream: SyncStreamName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<SyncCollectionName>,
    #[serde(default)]
    pub accepted: Vec<MutationId>,
    #[serde(default)]
    pub rejected: Vec<SyncRejectedMutation>,
    #[serde(default)]
    pub rows: Vec<SyncRow<T>>,
    #[serde(default)]
    pub conflicts: Vec<SyncConflict<T>>,
    pub cursor: Option<SyncCursor>,
}

impl<T> SyncPushResponse<T> {
    pub fn new(stream: SyncStreamName) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            collection: None,
            accepted: Vec::new(),
            rejected: Vec::new(),
            rows: Vec::new(),
            conflicts: Vec::new(),
            cursor: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_stream_names() {
        assert!(SyncStreamName::new("posts_for_tenant").is_ok());
        assert!(SyncStreamName::new("").is_err());
        assert!(SyncStreamName::new(" posts").is_err());
        assert!(SyncStreamName::new("posts\nbad").is_err());
        assert!(SyncStreamName::new("x".repeat(MAX_SYNC_TOKEN_LEN + 1)).is_err());
    }

    // RFC 088 §C: stream names must not contain `:` — the live-
    // wakeup wire protocol uses it as a structural delimiter, and
    // a sibling stream `issues:abcd1234abcd1234` would otherwise
    // collide with `issues`'s per-params topic format, breaking the
    // `LiveHub::allow_topic_prefixes` security boundary.
    #[test]
    fn rejects_colon_in_stream_name() {
        // 16-hex-suffix sibling: indistinguishable from per-params
        // format `query:sync:stream:issues:<hash>`.
        assert!(SyncStreamName::new("issues:abcd1234abcd1234").is_err());
        // Other colon-containing names also rejected.
        assert!(SyncStreamName::new("a:b").is_err());
        // Other token types may still use `:` (RowKey composite keys,
        // etc.). Only `stream` is protocol-reserved.
        assert!(RowKey::new("tenant:row_42").is_ok());
        assert!(MutationId::new("test:1").is_ok());
    }

    #[test]
    fn token_types_parse_and_borrow_as_str() {
        let stream: SyncStreamName = "posts_for_tenant".parse().unwrap();

        assert_eq!(stream.as_ref(), "posts_for_tenant");
        assert!("bad\nstream".parse::<SyncStreamName>().is_err());
    }

    #[test]
    fn validates_device_and_session_ids() {
        assert!(SyncDeviceId::new("device_abc").is_ok());
        assert!(SyncSessionId::new("session_abc").is_ok());
        assert!(SyncDeviceId::new("").is_err());
        assert!(SyncSessionId::new("session bad\n").is_err());
    }

    #[test]
    fn open_request_serializes_typed_client_id() {
        let request = SyncOpenRequest::new([SyncStreamName::new("posts").unwrap()])
            .client_id(SyncDeviceId::new("device_abc").unwrap());

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["client_id"], "device_abc");
    }

    #[test]
    fn deserializes_tokens_through_validation() {
        let stream: SyncStreamName = serde_json::from_str("\"posts_for_tenant\"").unwrap();
        assert_eq!(stream.as_str(), "posts_for_tenant");
        assert!(serde_json::from_str::<SyncStreamName>("\" posts\"").is_err());
        assert!(serde_json::from_str::<SyncStreamName>("\"posts\\nbad\"").is_err());
    }

    #[test]
    fn local_stream_key_is_bare_for_empty_params() {
        // Backwards-compat: pre-RFC-085 cache entries are keyed by
        // bare stream name. An empty params map must produce the same
        // key so existing on-disk snapshots keep hydrating.
        let stream = SyncStreamName::new("posts").unwrap();
        let key = local_stream_key(&stream, &BTreeMap::new());
        assert_eq!(key.as_str(), "posts");
    }

    #[test]
    fn local_stream_key_differs_per_distinct_params() {
        // Cache disambiguation: two subscriptions to the same stream
        // with different params produce different keys, so they don't
        // overwrite each other's cursor/rows/queue in the local store.
        let stream = SyncStreamName::new("issues").unwrap();
        let mut a = BTreeMap::new();
        a.insert("workspace_id".to_string(), Value::String("W1".to_string()));
        let mut b = BTreeMap::new();
        b.insert("workspace_id".to_string(), Value::String("W2".to_string()));
        let key_a = local_stream_key(&stream, &a);
        let key_b = local_stream_key(&stream, &b);
        assert_ne!(key_a, key_b);
        // Both still start with the wire stream name (debuggability).
        assert!(key_a.as_str().starts_with("issues__params_"));
        assert!(key_b.as_str().starts_with("issues__params_"));
    }

    #[test]
    fn local_stream_key_does_not_panic_for_max_length_stream_names() {
        // Regression: an `expect` in `local_stream_key` would fire when
        // `stream.len() + "__params_<16-hex>".len() > MAX_SYNC_TOKEN_LEN`.
        // Truncating the stream prefix at a char boundary keeps the
        // composite within budget without crashing.
        let long = "a".repeat(MAX_SYNC_TOKEN_LEN);
        let stream = SyncStreamName::new(long).unwrap();
        let mut params = BTreeMap::new();
        params.insert("k".to_string(), Value::String("v".to_string()));
        let key = local_stream_key(&stream, &params);
        assert!(key.as_str().len() <= MAX_SYNC_TOKEN_LEN);
        assert!(key.as_str().contains("__params_"));
    }

    #[test]
    fn local_stream_key_differs_for_long_stream_names_with_shared_prefix() {
        // Regression: a previous version hashed only the params, then
        // appended the truncated stream prefix. Two stream names that
        // shared the same first ~999 bytes would collapse to the same
        // composite key (cache cross-contamination at the local store).
        // The fix hashes the FULL stream + params so distinct streams
        // still get distinct keys even after prefix truncation.
        let prefix = "x".repeat(MAX_SYNC_TOKEN_LEN - 10);
        let stream_a = SyncStreamName::new(format!("{prefix}aaaaaaaaa")).unwrap();
        let stream_b = SyncStreamName::new(format!("{prefix}bbbbbbbbb")).unwrap();
        let mut params = BTreeMap::new();
        params.insert("k".to_string(), Value::String("v".to_string()));
        let key_a = local_stream_key(&stream_a, &params);
        let key_b = local_stream_key(&stream_b, &params);
        assert_ne!(
            key_a, key_b,
            "long stream names sharing a prefix must not collide"
        );
    }

    #[test]
    fn local_stream_key_is_stable_per_params() {
        // Determinism: the same params produce the same key across
        // calls (BTreeMap is sorted by key, so canonical JSON is
        // stable; FNV-1a has no random seed).
        let stream = SyncStreamName::new("issues").unwrap();
        let mut params = BTreeMap::new();
        params.insert("status".to_string(), serde_json::json!({"in": ["open"]}));
        params.insert("workspace_id".to_string(), Value::String("W".to_string()));
        let k1 = local_stream_key(&stream, &params);
        let k2 = local_stream_key(&stream, &params);
        assert_eq!(k1, k2);
    }

    #[test]
    fn sync_stream_tag_is_stable() {
        assert_eq!(
            sync_stream_tag("posts_for_tenant"),
            "sync:stream:posts_for_tenant"
        );
    }

    #[test]
    fn open_request_accepts_bare_stream_names_for_backwards_compat() {
        // Pre-RFC-085 clients serialized `streams` as `Vec<SyncStreamName>`,
        // which JSON-encodes as an array of strings. The new wire envelope
        // is `Vec<SyncStreamSubscription>`, but the custom deserializer
        // accepts bare strings too so old clients keep working against new
        // servers without coordination. See `SyncStreamSubscription`'s
        // `Deserialize` impl.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "streams": ["posts", "comments"],
        });
        let request: SyncOpenRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.streams.len(), 2);
        assert_eq!(request.streams[0].stream.as_str(), "posts");
        assert!(request.streams[0].params.is_empty());
        assert_eq!(request.streams[1].stream.as_str(), "comments");
        assert!(request.streams[1].params.is_empty());
    }

    #[test]
    fn open_request_round_trips_subscription_with_params() {
        // New client encodes params as part of each `SyncStreamSubscription`.
        // The deserializer accepts both the wrapped object form AND the
        // bare-string form, but the wrapped form is the canonical wire shape
        // for parametric subscriptions.
        let mut params = BTreeMap::new();
        params.insert("workspace_id".to_string(), Value::String("W".to_string()));
        let request = SyncOpenRequest::new([SyncStreamSubscription {
            stream: SyncStreamName::new("issues").unwrap(),
            params: params.clone(),
        }]);
        let json = serde_json::to_value(&request).unwrap();
        // Serialization keeps the object form when params are non-empty.
        assert_eq!(json["streams"][0]["stream"], "issues");
        assert_eq!(json["streams"][0]["params"]["workspace_id"], "W");

        // And deserialization recovers the same shape.
        let decoded: SyncOpenRequest = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.streams[0].stream.as_str(), "issues");
        assert_eq!(decoded.streams[0].params, params);
    }

    #[test]
    fn open_request_serializes_empty_params_as_bare_string() {
        // Backwards-compat with pre-RFC-085 servers: serialization
        // must emit the BARE STRING form (not an object with no
        // params) when params are empty, so an old server parsing
        // `Vec<SyncStreamName>` continues to deserialize cleanly.
        // Symmetric with `SyncStreamSubscription::Deserialize` which
        // accepts both shapes.
        let request = SyncOpenRequest::new([SyncStreamSubscription {
            stream: SyncStreamName::new("posts").unwrap(),
            params: BTreeMap::new(),
        }]);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["streams"][0], serde_json::json!("posts"));
    }

    #[test]
    fn open_request_subscription_accepts_explicit_null_params() {
        // Non-Rust clients (TS, Python) often emit `null` for unset
        // map-typed fields. The custom deserializer must coerce
        // `params: null` to an empty map.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "streams": [{"stream": "posts", "params": null}],
        });
        let request: SyncOpenRequest = serde_json::from_value(json).unwrap();
        assert!(request.streams[0].params.is_empty());
    }

    #[test]
    fn open_request_subscription_rejects_unknown_field_in_object_form() {
        // The previous untagged-enum Deserialize silently fell back to
        // empty params on a typo like `param` vs `params`. The custom
        // visitor surfaces an explicit "unknown field" error instead.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "streams": [{"stream": "posts", "param": {"workspace_id": "W"}}],
        });
        let err = serde_json::from_value::<SyncOpenRequest>(json).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field") && msg.contains("param"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn pull_request_accepts_explicit_null_params() {
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "params": null,
            "cursor": null,
            "limit": 500,
        });
        let request: SyncPullRequest = serde_json::from_value(json).unwrap();
        assert!(request.params.is_empty());
    }

    #[test]
    fn push_request_accepts_explicit_null_params() {
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "params": null,
            "mutations": [],
        });
        let request: SyncPushRequest<Value> = serde_json::from_value(json).unwrap();
        assert!(request.params.is_empty());
    }

    #[test]
    fn pull_request_params_default_empty() {
        // Old servers respond to `/pull` with the legacy envelope shape.
        // A new client deserializing a server response with no `params`
        // field must succeed with an empty map.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "cursor": null,
            "limit": 500,
        });
        let request: SyncPullRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.stream.as_str(), "posts");
        assert!(request.params.is_empty());
    }

    #[test]
    fn push_request_params_default_empty() {
        // Old client envelope (no params field) deserializes cleanly into
        // the new struct with `params: {}`.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "mutations": [],
        });
        let request: SyncPushRequest<Value> = serde_json::from_value(json).unwrap();
        assert_eq!(request.stream.as_str(), "posts");
        assert!(request.params.is_empty());
    }

    #[test]
    fn open_stream_schema_version_accepts_missing_and_explicit_null() {
        // Missing field — the existing `#[serde(default)]` already
        // handled this; lock the behaviour against future changes.
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert_eq!(stream.schema_version, 1);

        // Explicit JSON null — the new `deserialize_with` coerces it
        // to the default. Without this, non-Rust clients emitting
        // `{"schema_version": null}` would fail with `invalid type:
        // null, expected u32`.
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
            "schema_version": null,
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert_eq!(stream.schema_version, 1);

        // A concrete value still round-trips.
        let json = serde_json::json!({
            "stream": "posts",
            "collection": "posts",
            "cursor": null,
            "schema_version": 7,
        });
        let stream: SyncOpenStream = serde_json::from_value(json).unwrap();
        assert_eq!(stream.schema_version, 7);
    }

    #[test]
    fn push_request_schema_version_accepts_missing_and_explicit_null() {
        let stream = SyncStreamName::new("posts").unwrap();
        // Missing → default 1.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "mutations": [],
        });
        let req: SyncPushRequest<serde_json::Value> = serde_json::from_value(json).unwrap();
        assert_eq!(req.schema_version, 1);
        let _ = stream;
        // Explicit null → coerced to default 1.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "mutations": [],
            "schema_version": null,
        });
        let req: SyncPushRequest<serde_json::Value> = serde_json::from_value(json).unwrap();
        assert_eq!(req.schema_version, 1);
    }

    #[test]
    fn with_schema_version_coerces_zero_to_default() {
        let stream = SyncStreamName::new("posts").unwrap();
        let req: SyncPushRequest<serde_json::Value> = SyncPushRequest::new(stream, []);
        let req = req.with_schema_version(0);
        // Builder coerces 0 → 1 so a stray default never lands on the wire.
        assert_eq!(req.schema_version, 1);
    }

    #[test]
    fn sync_row_helpers_attach_version_and_flags() {
        let version = RowVersion::new("row_1").unwrap();
        let row = SyncRow::new("post_1", "hello".to_string())
            .unwrap()
            .row_version(version.clone())
            .pending(true)
            .conflict(true);

        assert_eq!(row.version, Some(version));
        assert!(row.pending);
        assert!(row.conflict);
    }

    #[test]
    fn client_mutation_helpers_build_row_scoped_mutations() {
        let id = MutationId::new("device_abc:1").unwrap();
        let version = RowVersion::new("row_1").unwrap();

        let mutation = ClientMutation::upsert_row(id.clone(), "post_1", "payload")
            .unwrap()
            .base_row_version(Some(version.clone()));

        assert_eq!(mutation.id, id);
        assert_eq!(mutation.key.unwrap().as_str(), "post_1");
        assert_eq!(mutation.op, SyncOp::Upsert);
        assert_eq!(mutation.base_version, Some(version));
        assert_eq!(mutation.payload, "payload");
    }

    #[test]
    fn client_mutation_draft_helpers_build_row_scoped_mutations() {
        let id = MutationId::new("device_abc:1").unwrap();
        let version = RowVersion::new("row_1").unwrap();

        let mutation = ClientMutationDraft::delete_row("post_1", ())
            .unwrap()
            .base_row_version(Some(version.clone()))
            .with_id(id.clone());

        assert_eq!(mutation.id, id);
        assert_eq!(mutation.key.unwrap().as_str(), "post_1");
        assert_eq!(mutation.op, SyncOp::Delete);
        assert_eq!(mutation.base_version, Some(version));
    }

    // ---- RFC 088 §C: stream_params_hash + sync_stream_params_tag ---
    //
    // These tests double as the cross-language hash spec. The
    // `stream_params_hash_golden_fixture` test pins a set of
    // (stream, params) → hex-hash pairs. Future ports (a Swift
    // client, a Go server, etc.) MUST reproduce these exact bytes
    // or the per-(stream, params) live-wakeup routing silently
    // breaks.

    #[test]
    fn stream_params_tag_format() {
        let tag = sync_stream_params_tag("issues", 0xABCD_1234_5678_9ABC);
        assert_eq!(tag, "sync:stream:issues:abcd123456789abc");
    }

    #[test]
    fn params_hash_hex_len_matches_formatter() {
        // The formatter in `sync_stream_params_tag` hard-codes
        // `{:016x}`. `PARAMS_HASH_HEX_LEN` must match — otherwise
        // the live validator (`pocopine_live::is_params_hash_suffix`)
        // would gate the wrong byte-count and reject valid topics
        // (or accept malformed ones).
        let tag = sync_stream_params_tag("s", 0);
        let suffix = tag.rsplit_once(':').unwrap().1;
        assert_eq!(suffix.len(), PARAMS_HASH_HEX_LEN);
    }

    #[test]
    fn stream_params_tag_pads_to_16_hex() {
        // Small hashes must still be 16 hex chars (zero-padded) so
        // the topic format is uniform across clients.
        let tag = sync_stream_params_tag("issues", 0xFFu64);
        assert_eq!(tag, "sync:stream:issues:00000000000000ff");
    }

    #[test]
    fn stream_params_hash_empty_params_distinguishes_streams() {
        let h_issues = stream_params_hash("issues", &StreamParams::new());
        let h_comments = stream_params_hash("comments", &StreamParams::new());
        assert_ne!(
            h_issues, h_comments,
            "stream name folds into the hash; empty params alone must not alias"
        );
    }

    #[test]
    fn stream_params_hash_separator_avoids_prefix_collision() {
        // "ab" + {} vs "a" + {"b": null}: without the separator
        // these would have identical byte sequences. With the 0x00
        // separator they don't.
        let mut p2 = StreamParams::new();
        p2.insert("b".into(), Value::Null);
        let h1 = stream_params_hash("ab", &StreamParams::new());
        let h2 = stream_params_hash("a", &p2);
        assert_ne!(h1, h2);
    }

    #[test]
    fn stream_params_hash_is_stable_across_insert_order() {
        // StreamParams is a BTreeMap — sorted-by-key by construction.
        // Inserting in different orders must produce identical hashes.
        let mut p_in_order = StreamParams::new();
        p_in_order.insert("a".into(), json!("1"));
        p_in_order.insert("b".into(), json!("2"));
        p_in_order.insert("c".into(), json!("3"));

        let mut p_reversed = StreamParams::new();
        p_reversed.insert("c".into(), json!("3"));
        p_reversed.insert("a".into(), json!("1"));
        p_reversed.insert("b".into(), json!("2"));

        assert_eq!(
            stream_params_hash("issues", &p_in_order),
            stream_params_hash("issues", &p_reversed),
        );
    }

    #[test]
    fn stream_params_hash_distinguishes_distinct_values() {
        let mut p1 = StreamParams::new();
        p1.insert("workspace_id".into(), json!("W1"));
        let mut p2 = StreamParams::new();
        p2.insert("workspace_id".into(), json!("W2"));
        assert_ne!(
            stream_params_hash("issues", &p1),
            stream_params_hash("issues", &p2),
        );
    }

    /// **Golden fixture** — DO NOT EDIT VALUES.
    ///
    /// These hex-encoded hashes are the wire-protocol contract for
    /// RFC 088 §C. Any port (server-side hash producer, client-side
    /// hash subscriber) must produce exactly these bytes for these
    /// inputs, or the live-wakeup topic routing breaks silently
    /// (mutations get published to topic X, clients listen on topic
    /// X', no overlap, no error).
    ///
    /// If a future change MUST alter the hash function, bump
    /// `LIVE_PROTOCOL_V1` first and add migration logic.
    #[test]
    #[allow(clippy::type_complexity)] // golden-fixture-only triple
    fn stream_params_hash_golden_fixture() {
        // (stream, params_kvs, expected_hex_hash)
        let cases: &[(&str, &[(&str, Value)], &str)] = &[
            // 1. bare stream, empty params
            ("issues", &[], "issues_empty"),
            // 2. single param, string value
            ("issues", &[("workspace_id", json!("W1"))], "issues_w1"),
            // 3. two params, sorted order
            (
                "issues",
                &[("status", json!("open")), ("workspace_id", json!("W1"))],
                "issues_w1_open",
            ),
            // 4. nested object value (rare but supported)
            (
                "comments",
                &[("filter", json!({"status": "active"}))],
                "comments_nested",
            ),
            // 5. distinct stream name with same params as #2
            ("comments", &[("workspace_id", json!("W1"))], "comments_w1"),
        ];

        let mut computed: Vec<(String, u64)> = Vec::new();
        for (stream, kvs, label) in cases {
            let mut params = StreamParams::new();
            for (k, v) in *kvs {
                params.insert((*k).to_string(), v.clone());
            }
            computed.push(((*label).into(), stream_params_hash(stream, &params)));
        }

        // The expected hashes are pinned by the snapshot below.
        // Pretty-printed so a diff in CI shows exactly which case
        // drifted.
        let snapshot: String = computed
            .iter()
            .map(|(label, hash)| format!("{label} = {hash:016x}"))
            .collect::<Vec<_>>()
            .join("\n");

        let expected = "\
issues_empty = ebb76190a8dc90b7
issues_w1 = 6937b67bfe60e730
issues_w1_open = 35eea32b30a1d59c
comments_nested = ba8db781db55988d
comments_w1 = 668b4b2f8b17847e";

        assert_eq!(
            snapshot, expected,
            "RFC 088 §C wire-protocol hash spec drifted. \
             If this drift is intentional, bump LIVE_PROTOCOL_V1 \
             FIRST and add migration logic — old clients hashing \
             the old way will MISS wakeups until they upgrade."
        );
    }

    // -----------------------------------------------------------------

    #[test]
    fn endpoint_paths_share_prefix() {
        assert!(SYNC_OPEN_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_OPEN_PATH.ends_with("/open"));
        assert!(SYNC_PULL_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_PULL_PATH.ends_with("/pull"));
        assert!(SYNC_PUSH_PATH.starts_with(SYNC_ENDPOINT_PREFIX));
        assert!(SYNC_PUSH_PATH.ends_with("/push"));
    }
}
