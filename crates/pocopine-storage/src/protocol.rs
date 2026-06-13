use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;

use crate::{StorageError, StorageResult};

/// Current storage protocol identifier.
pub const STORAGE_PROTOCOL_V1: &str = "pocopine.storage.v1";
/// Maximum length, in bytes, accepted for small protocol token strings.
pub const MAX_STORAGE_TOKEN_LEN: usize = 1024;

macro_rules! storage_path {
    ($suffix:literal) => {
        concat!("/__pocopine/storage/v1", $suffix)
    };
}

/// Default storage endpoint prefix mounted by the server plugin.
pub const STORAGE_ENDPOINT_PREFIX: &str = storage_path!("");
/// Scope descriptor route prefix.
pub const STORAGE_SCOPES_PREFIX: &str = storage_path!("/scopes");
/// Upload route prefix.
pub const STORAGE_UPLOADS_PREFIX: &str = storage_path!("/uploads");
/// Upload creation route.
pub const STORAGE_UPLOADS_PATH: &str = storage_path!("/uploads");
/// Default tus 1.0 endpoint prefix mounted by the tus server plugin.
pub const STORAGE_TUS_ENDPOINT_PREFIX: &str = "/__pocopine/storage/tus/v1";
/// Anonymous upload binding cookie read by the storage server.
pub const STORAGE_ANON_COOKIE: &str = "pocopine_storage_anon";

/// Stable JSON envelope used by storage HTTP routes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageResponse<T> {
    pub ok: bool,
    #[serde(default = "none", skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<StorageError>,
}

impl<T> StorageResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(error: StorageError) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(error),
        }
    }

    pub fn from_result(result: StorageResult<T>) -> Self {
        match result {
            Ok(data) => Self::ok(data),
            Err(error) => Self::err(error),
        }
    }

    pub fn into_result(self) -> StorageResult<T> {
        if self.ok {
            self.data
                .ok_or_else(|| StorageError::client("storage response omitted data"))
        } else {
            Err(self
                .error
                .unwrap_or_else(|| StorageError::client("storage response omitted error")))
        }
    }
}

fn none<T>() -> Option<T> {
    None
}

fn validate_token(field: &'static str, value: String) -> StorageResult<String> {
    let trimmed = value.trim();
    let bytes = value.as_bytes();
    let all_allowed = bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    // Require at least one alphanumeric byte so "." and ".." (and any
    // all-punctuation value) cannot be accepted. Without this, the value
    // can be used as a filesystem path component and traverse out of
    // backend session roots.
    let has_alphanumeric = bytes.iter().any(u8::is_ascii_alphanumeric);
    if value.len() > MAX_STORAGE_TOKEN_LEN
        || trimmed.is_empty()
        || trimmed != value
        || !all_allowed
        || !has_alphanumeric
    {
        return Err(StorageError::invalid_value(field, value));
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
            pub fn new(value: impl Into<String>) -> StorageResult<Self> {
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
            type Err = StorageError;

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
            type Error = StorageError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = StorageError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

opaque_string_type!(
    StorageBackendName,
    "backend",
    "Server-registered storage backend name."
);
opaque_string_type!(
    UploadSessionId,
    "upload session",
    "Opaque upload session identifier."
);

/// Authenticated actor reference captured for storage session ownership.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PrincipalRef {
    pub subject: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// Anonymous browser binding captured for public upload ownership.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnonymousUploadBinding {
    pub id: String,
}

/// Provider-safe object key. This is not a filesystem path.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SafeObjectKey(String);

impl SafeObjectKey {
    /// Parse and validate an object key.
    pub fn parse(key: impl AsRef<str>) -> StorageResult<Self> {
        let key = key.as_ref();
        if key.is_empty()
            || key.starts_with('/')
            || key.starts_with('\\')
            || key.contains('\\')
            || key.chars().any(char::is_control)
        {
            return Err(StorageError::invalid_value("object key", key));
        }

        for segment in key.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(StorageError::invalid_value("object key", key));
            }
            if segment
                .chars()
                .any(|ch| matches!(ch, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
                || segment.ends_with(' ')
                || segment.ends_with('.')
                || is_windows_reserved_segment(segment)
            {
                return Err(StorageError::invalid_value("object key", key));
            }
        }

        if key.split('/').next().is_some_and(|segment| {
            segment.eq_ignore_ascii_case("__pocopine")
                || segment.eq_ignore_ascii_case(".pocopine")
                || segment.eq_ignore_ascii_case(".pocopine-storage")
        }) {
            return Err(StorageError::invalid_value("object key", key));
        }

        Ok(Self(key.to_string()))
    }

    /// Borrow the key as a string.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

fn is_windows_reserved_segment(segment: &str) -> bool {
    let stem = segment.split_once('.').map_or(segment, |(stem, _)| stem);
    let stem = stem.trim_end_matches([' ', '.']);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

impl fmt::Display for SafeObjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for SafeObjectKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Serialize for SafeObjectKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SafeObjectKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Visibility selected for a completed object.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ObjectVisibility {
    #[default]
    Private,
    Public,
}

/// Upload strategy requested by a client or selected by the backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UploadStrategy {
    Auto,
    SingleRequest,
    Sequential,
    Multipart,
}

/// The upload mechanisms a storage backend supports, advertised so the server
/// can negotiate a requested [`UploadStrategy`] against what the backend can
/// actually do (see [`crate::backend_common::select_upload_mode`]).
///
/// Backends that do not override `StorageBackend::capabilities()` advertise the
/// [`Default`] — sequential proxy only — so adding a new capability never
/// silently changes an existing backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct BackendCapabilities {
    /// Ordered `PATCH`-chunk proxy uploads streamed through the server.
    pub sequential_proxy: bool,
    /// Server-mediated provider-native multipart (parts assembled server-side).
    pub native_multipart: bool,
    /// The whole object accepted in a single request.
    pub single_request: bool,
    /// Signed direct-to-provider part URLs (the browser uploads bypass the
    /// server). Opt-in per scope via the upload policy.
    pub signed_direct: bool,
}

impl Default for BackendCapabilities {
    fn default() -> Self {
        Self {
            sequential_proxy: true,
            native_multipart: false,
            single_request: false,
            signed_direct: false,
        }
    }
}

impl BackendCapabilities {
    /// A backend that only supports sequential proxy uploads (the default).
    pub fn sequential_only() -> Self {
        Self::default()
    }

    /// Advertise server-mediated native multipart (the by-number `upload_part`
    /// path) in addition to whatever is already set. Builder helper so external
    /// backend crates can opt into a capability without naming every field of
    /// this `#[non_exhaustive]` struct.
    pub fn with_native_multipart(mut self) -> Self {
        self.native_multipart = true;
        self
    }

    /// The strategies a client may request against a backend with these
    /// capabilities, used to populate the scope descriptor so capability
    /// discovery matches what `initiate_upload` actually accepts.
    pub fn strategies(&self) -> Vec<UploadStrategy> {
        let mut strategies = Vec::new();
        if self.sequential_proxy {
            strategies.push(UploadStrategy::Sequential);
        }
        if self.native_multipart {
            strategies.push(UploadStrategy::Multipart);
        }
        if self.single_request {
            strategies.push(UploadStrategy::SingleRequest);
        }
        strategies
    }
}

/// Public upload session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UploadSessionStatus {
    Open,
    Completing,
    Complete,
    Aborted,
    Expired,
}

/// Upload progress phase reported by the browser client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UploadPhase {
    Initiating,
    Uploading,
    Retrying,
    Completing,
    Complete,
    Aborted,
    Failed,
}

/// Upload progress event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UploadProgress {
    pub bytes_sent: u64,
    pub bytes_total: Option<u64>,
    pub current_part: Option<u32>,
    pub phase: UploadPhase,
}

/// Checksum policy advertised by a scope/backend.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChecksumPolicy {
    #[default]
    None,
    Optional(Vec<ChecksumAlgorithm>),
    Required(ChecksumAlgorithm),
}

/// Supported checksum algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChecksumAlgorithm {
    Sha256,
    Crc32c,
    Md5,
}

/// Completed object checksum.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectChecksum {
    pub algorithm: ChecksumAlgorithm,
    pub value: String,
}

/// Server-authoritative upload policy for one scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadPolicy {
    pub backend: StorageBackendName,
    pub max_bytes: u64,
    pub allowed_content_types: Vec<String>,
    pub allowed_extensions: Vec<String>,
    pub max_files_per_batch: u32,
    pub visibility: ObjectVisibility,
    pub checksum: ChecksumPolicy,
    pub expires_after: Duration,
    pub metadata_schema: MetadataSchema,
    pub resumable: bool,
    pub preferred_chunk_size: Option<u64>,
    pub min_part_size: Option<u64>,
    pub max_part_size: Option<u64>,
    pub max_parts: Option<u32>,
    pub max_concurrent_parts: u16,
}

impl UploadPolicy {
    pub fn new(backend: impl Into<String>) -> StorageResult<Self> {
        Ok(Self {
            backend: StorageBackendName::new(backend.into())?,
            max_bytes: 10 * 1024 * 1024,
            allowed_content_types: Vec::new(),
            allowed_extensions: Vec::new(),
            max_files_per_batch: 1,
            visibility: ObjectVisibility::Private,
            checksum: ChecksumPolicy::None,
            expires_after: Duration::from_secs(60 * 60),
            metadata_schema: MetadataSchema::default(),
            resumable: true,
            preferred_chunk_size: Some(1024 * 1024),
            min_part_size: None,
            max_part_size: None,
            max_parts: None,
            max_concurrent_parts: 1,
        })
    }

    pub fn max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn allowed_content_types<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_content_types = values.into_iter().map(Into::into).collect();
        self
    }

    pub fn allowed_extensions<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_extensions = values.into_iter().map(Into::into).collect();
        self
    }

    pub fn preferred_chunk_size(mut self, preferred_chunk_size: u64) -> Self {
        self.preferred_chunk_size = Some(preferred_chunk_size);
        self
    }

    pub fn visibility(mut self, visibility: ObjectVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn validate_configuration(&self) -> StorageResult<()> {
        if self.max_bytes == 0 {
            return Err(StorageError::policy_rejected(
                "upload policy max_bytes must be greater than zero",
            ));
        }
        if self.max_concurrent_parts == 0 {
            return Err(StorageError::policy_rejected(
                "upload policy max_concurrent_parts must be greater than zero",
            ));
        }
        if let Some(max_parts) = self.max_parts
            && max_parts == 0 {
                return Err(StorageError::policy_rejected(
                    "upload policy max_parts must be greater than zero",
                ));
            }
        if let (Some(min), Some(preferred)) = (self.min_part_size, self.preferred_chunk_size)
            && min > preferred {
                return Err(StorageError::policy_rejected(
                    "upload policy min_part_size cannot exceed preferred_chunk_size",
                ));
            }
        if let (Some(preferred), Some(max)) = (self.preferred_chunk_size, self.max_part_size)
            && preferred > max {
                return Err(StorageError::policy_rejected(
                    "upload policy preferred_chunk_size cannot exceed max_part_size",
                ));
            }
        if let (Some(min), Some(max)) = (self.min_part_size, self.max_part_size)
            && min > max {
                return Err(StorageError::policy_rejected(
                    "upload policy min_part_size cannot exceed max_part_size",
                ));
            }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn validate_initiate(&self, request: &InitiateUploadRequest) -> StorageResult<()> {
        if let Some(size) = request.size
            && size > self.max_bytes {
                return Err(StorageError::payload_too_large(self.max_bytes));
            }

        if !self.allowed_content_types.is_empty() {
            let content_type = request
                .content_type
                .as_deref()
                .ok_or_else(|| StorageError::policy_rejected("missing content type"))?;
            if !self
                .allowed_content_types
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(content_type))
            {
                return Err(StorageError::policy_rejected("content type is not allowed"));
            }
        }

        if !self.allowed_extensions.is_empty() {
            let extension = file_extension(&request.file_name)
                .ok_or_else(|| StorageError::policy_rejected("missing file extension"))?;
            if !self.allowed_extensions.iter().any(|allowed| {
                allowed
                    .trim_start_matches('.')
                    .eq_ignore_ascii_case(extension)
            }) {
                return Err(StorageError::policy_rejected(
                    "file extension is not allowed",
                ));
            }
        }

        self.metadata_schema.validate(&request.metadata)?;
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn descriptor(
        &self,
        scope: &str,
        strategies: Vec<UploadStrategy>,
    ) -> UploadPolicyDescriptor {
        UploadPolicyDescriptor {
            protocol: STORAGE_PROTOCOL_V1.to_string(),
            scope: scope.to_string(),
            max_bytes: self.max_bytes,
            allowed_content_types: self.allowed_content_types.clone(),
            allowed_extensions: self.allowed_extensions.clone(),
            max_files_per_batch: self.max_files_per_batch,
            supports_progress: true,
            supports_abort: true,
            supports_batch: false,
            strategies,
            preferred_chunk_size: self.preferred_chunk_size,
            min_part_size: self.min_part_size,
            max_part_size: self.max_part_size,
            max_parts: self.max_parts,
            max_concurrent_parts: self.max_concurrent_parts,
        }
    }
}

/// Safe policy projection returned to the browser.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UploadPolicyDescriptor {
    pub protocol: String,
    pub scope: String,
    pub max_bytes: u64,
    pub allowed_content_types: Vec<String>,
    pub allowed_extensions: Vec<String>,
    pub max_files_per_batch: u32,
    pub supports_progress: bool,
    pub supports_abort: bool,
    pub supports_batch: bool,
    pub strategies: Vec<UploadStrategy>,
    pub preferred_chunk_size: Option<u64>,
    pub min_part_size: Option<u64>,
    pub max_part_size: Option<u64>,
    pub max_parts: Option<u32>,
    pub max_concurrent_parts: u16,
}

/// Metadata validation policy for simple string metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataSchema {
    pub allowed_keys: Vec<String>,
    pub max_key_bytes: usize,
    pub max_value_bytes: usize,
}

impl Default for MetadataSchema {
    fn default() -> Self {
        Self {
            allowed_keys: Vec::new(),
            max_key_bytes: 128,
            max_value_bytes: 1024,
        }
    }
}

impl MetadataSchema {
    #[cfg(not(target_arch = "wasm32"))]
    fn validate(&self, metadata: &BTreeMap<String, String>) -> StorageResult<()> {
        for (key, value) in metadata {
            if key.is_empty() || key.len() > self.max_key_bytes || key.chars().any(char::is_control)
            {
                return Err(StorageError::policy_rejected("metadata key is invalid"));
            }
            if value.len() > self.max_value_bytes || value.chars().any(char::is_control) {
                return Err(StorageError::policy_rejected("metadata value is invalid"));
            }
            if !self.allowed_keys.is_empty()
                && !self.allowed_keys.iter().any(|allowed| allowed == key)
            {
                return Err(StorageError::policy_rejected("metadata key is not allowed"));
            }
        }
        Ok(())
    }
}

/// App-owned storage key plus ownership metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageKey {
    pub key: SafeObjectKey,
    pub owner: Option<ObjectOwnerRef>,
    pub metadata: ObjectMetadata,
}

impl StorageKey {
    pub fn new(key: SafeObjectKey) -> Self {
        Self {
            key,
            owner: None,
            metadata: ObjectMetadata::default(),
        }
    }

    pub fn owner(mut self, owner: ObjectOwnerRef) -> Self {
        self.owner = Some(owner);
        self
    }

    pub fn metadata(mut self, metadata: ObjectMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Domain owner reference attached by an application key resolver.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectOwnerRef {
    pub kind: String,
    pub id: String,
}

impl ObjectOwnerRef {
    pub fn principal(id: impl Into<String>) -> Self {
        Self {
            kind: "principal".to_string(),
            id: id.into(),
        }
    }
}

/// Provider-neutral object metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectMetadata(pub BTreeMap<String, String>);

impl ObjectMetadata {
    pub fn from_entries<K, V>(entries: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        Self(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }
}

impl<K, V> FromIterator<(K, V)> for ObjectMetadata
where
    K: Into<String>,
    V: Into<String>,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self::from_entries(iter)
    }
}

/// Provider-neutral value applications store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub backend: String,
    pub scope: String,
    pub key: String,
    pub version: Option<String>,
    pub etag: Option<String>,
    pub checksum: Option<ObjectChecksum>,
    pub content_type: Option<String>,
    pub size: u64,
    pub visibility: ObjectVisibility,
    pub metadata: BTreeMap<String, String>,
}

/// Public upload session view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UploadSession {
    pub id: UploadSessionId,
    pub scope: String,
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    pub strategy: UploadStrategy,
    pub status: UploadSessionStatus,
    pub next_offset: Option<u64>,
    pub part_size: Option<u64>,
    pub plan: TransferPlan,
    pub uploaded_parts: Vec<UploadedPartView>,
    pub expires_at: OffsetDateTime,
}

/// Transfer limits selected for a session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransferPlan {
    pub min_part_size: Option<u64>,
    pub preferred_part_size: Option<u64>,
    pub max_part_size: Option<u64>,
    pub max_parts: Option<u32>,
    pub max_concurrent_parts: u16,
    pub resumable: bool,
}

/// Absolute ceiling on the number of parts a multipart upload may plan, applied
/// when the plan advertises no `max_parts`. Above every provider's real limit
/// (S3 10k, GCS 32, Azure 50k) yet far below `u32::MAX`, so an untrusted or
/// corrupted plan can never drive an unbounded allocation or overflow a part
/// number.
const MAX_MULTIPART_PARTS: u64 = 100_000;

/// One planned multipart part: its 1-based number and byte range in the source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartSpec {
    pub number: u32,
    pub offset: u64,
    pub len: u64,
}

/// The part size a client should use for a multipart upload under `plan`: the
/// preferred size (or `max_part_size` as a fallback) clamped into `[min, max]`.
/// The native backends advertise `preferred == max == part_size`, so this
/// reproduces the exact part size the server's completion enforces.
///
/// Fails loud rather than guessing: a plan that advertises no part size at all
/// is rejected, so a misconfigured backend surfaces immediately instead of the
/// client silently slicing at some default and having every part rejected.
fn multipart_part_size(plan: &TransferPlan) -> StorageResult<u64> {
    let preferred = plan
        .preferred_part_size
        .or(plan.max_part_size)
        .ok_or_else(|| {
            StorageError::policy_rejected("multipart upload plan advertises no part size")
        })?;
    let min = plan.min_part_size.unwrap_or(1).max(1);
    let max = plan.max_part_size.unwrap_or(u64::MAX).max(min);
    Ok(preferred.max(1).clamp(min, max))
}

/// Lay out a multipart upload of `size` bytes against a session's
/// [`TransferPlan`]: parts of `multipart_part_size(plan)` each, the last the
/// remainder, numbered from 1. A zero-byte upload plans no parts (the server
/// completes it as an empty object).
///
/// This is the shared, target-neutral sizing the browser client uses to drive
/// the by-number part endpoint — kept here (not in the wasm client) so host
/// tests cover it and it stays in lock-step with the backends' part layout.
/// Rejects (rather than allocating/overflowing on) an upload that would need
/// more parts than the plan's `max_parts` — or [`MAX_MULTIPART_PARTS`] when the
/// plan sets none — so an untrusted plan can't drive an unbounded `Vec` or a
/// part-number overflow.
pub fn plan_parts(size: u64, plan: &TransferPlan) -> StorageResult<Vec<PartSpec>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    let part_size = multipart_part_size(plan)?;
    let count = size.div_ceil(part_size);
    let max_parts = plan
        .max_parts
        .map_or(MAX_MULTIPART_PARTS, u64::from)
        .min(MAX_MULTIPART_PARTS);
    if count > max_parts {
        return Err(StorageError::policy_rejected(format!(
            "upload of {size} bytes needs {count} parts, exceeding the {max_parts}-part limit"
        )));
    }
    // `count <= max_parts <= MAX_MULTIPART_PARTS`, so the capacity is bounded and
    // the 1-based `number` (`u32`) cannot overflow.
    let mut parts = Vec::with_capacity(count as usize);
    let mut offset = 0u64;
    let mut number = 1u32;
    while offset < size {
        let len = part_size.min(size - offset);
        parts.push(PartSpec {
            number,
            offset,
            len,
        });
        offset += len;
        number += 1;
    }
    Ok(parts)
}

/// Browser-safe multipart part view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UploadedPartView {
    pub number: u32,
    pub size: u64,
    pub status: UploadedPartStatus,
    pub checksum: Option<ObjectChecksum>,
}

/// Public multipart part status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UploadedPartStatus {
    Prepared,
    Uploaded,
    Committed,
}

/// Signed read target placeholder for later read routes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignedRead {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub expires_at: OffsetDateTime,
}

/// Browser upload creation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InitiateUploadRequest {
    pub protocol: String,
    pub scope: String,
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    pub requested_strategy: UploadStrategy,
}

impl InitiateUploadRequest {
    pub fn new(scope: impl Into<String>, file_name: impl Into<String>) -> Self {
        Self {
            protocol: STORAGE_PROTOCOL_V1.to_string(),
            scope: scope.into(),
            file_name: file_name.into(),
            size: None,
            content_type: None,
            metadata: BTreeMap::new(),
            requested_strategy: UploadStrategy::Auto,
        }
    }
}

/// Server-built upload intent passed to app key resolvers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadIntent {
    pub scope: String,
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub requested_strategy: UploadStrategy,
    pub generated_object_id: String,
}

impl UploadIntent {
    pub fn generated_object_id(&self) -> &str {
        self.generated_object_id.as_str()
    }

    pub fn file_name(&self) -> &str {
        self.file_name.as_str()
    }

    pub fn extension(&self) -> Option<&str> {
        file_extension(&self.file_name)
    }
}

/// Backend upload initiation request.
#[derive(Clone, Debug)]
pub struct InitiateUpload {
    pub scope: String,
    pub storage_key: StorageKey,
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub requested_strategy: UploadStrategy,
    pub policy: UploadPolicy,
}

/// Complete-upload request body.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompleteUploadRequest {
    #[serde(default)]
    pub checksum: Option<ObjectChecksum>,
}

/// Backend complete-upload request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteUpload {
    pub session: UploadSessionId,
    pub checksum: Option<ObjectChecksum>,
}

/// Direct and multipart target modes are protocol variants in PR 1, but the
/// first implementation only executes sequential proxy uploads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UploadTarget {
    Direct {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        expires_at: OffsetDateTime,
    },
    Proxy {
        url: String,
        headers: Vec<(String, String)>,
    },
}

pub(crate) fn file_extension(file_name: &str) -> Option<&str> {
    file_name
        .rsplit_once('.')
        .and_then(|(_, extension)| (!extension.is_empty()).then_some(extension))
}

#[cfg(test)]
mod plan_parts_tests {
    use super::*;

    fn plan(preferred: u64, min: u64, max: u64, max_parts: Option<u32>) -> TransferPlan {
        TransferPlan {
            min_part_size: Some(min),
            preferred_part_size: Some(preferred),
            max_part_size: Some(max),
            max_parts,
            max_concurrent_parts: 4,
            resumable: true,
        }
    }

    #[test]
    fn empty_upload_plans_no_parts() {
        assert!(plan_parts(0, &plan(5, 5, 5, Some(10))).unwrap().is_empty());
    }

    #[test]
    fn exact_multiple_splits_into_equal_parts() {
        let parts = plan_parts(15, &plan(5, 5, 5, Some(10))).unwrap();
        assert_eq!(
            parts,
            vec![
                PartSpec {
                    number: 1,
                    offset: 0,
                    len: 5
                },
                PartSpec {
                    number: 2,
                    offset: 5,
                    len: 5
                },
                PartSpec {
                    number: 3,
                    offset: 10,
                    len: 5
                },
            ]
        );
    }

    #[test]
    fn remainder_is_the_last_part() {
        let parts = plan_parts(12, &plan(5, 5, 5, Some(10))).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts[2],
            PartSpec {
                number: 3,
                offset: 10,
                len: 2
            }
        );
        // total length covers the whole object exactly, contiguously
        assert_eq!(parts.iter().map(|p| p.len).sum::<u64>(), 12);
        let mut next = 0;
        for p in &parts {
            assert_eq!(p.offset, next);
            next += p.len;
        }
    }

    #[test]
    fn single_part_when_smaller_than_part_size() {
        let parts = plan_parts(3, &plan(5, 5, 5, Some(10))).unwrap();
        assert_eq!(
            parts,
            vec![PartSpec {
                number: 1,
                offset: 0,
                len: 3
            }]
        );
    }

    #[test]
    fn preferred_is_clamped_into_min_max() {
        // preferred below min → clamps up to min (4)
        assert_eq!(multipart_part_size(&plan(1, 4, 16, None)).unwrap(), 4);
        // preferred above max → clamps down to max (8)
        assert_eq!(multipart_part_size(&plan(64, 4, 8, None)).unwrap(), 8);
    }

    #[test]
    fn exceeding_max_parts_is_rejected() {
        // 100 bytes / 5-byte parts = 20 parts, over a 10-part cap
        let err = plan_parts(100, &plan(5, 5, 5, Some(10))).unwrap_err();
        assert!(matches!(err, StorageError::PolicyRejected { .. }));
    }

    #[test]
    fn plan_with_no_part_size_is_rejected_not_guessed() {
        let no_size = TransferPlan {
            min_part_size: None,
            preferred_part_size: None,
            max_part_size: None,
            max_parts: None,
            max_concurrent_parts: 4,
            resumable: true,
        };
        // A non-empty upload must fail loud rather than slice at a guessed size.
        assert!(matches!(
            plan_parts(10, &no_size),
            Err(StorageError::PolicyRejected { .. })
        ));
        // An empty upload still needs no parts and never consults the size.
        assert!(plan_parts(0, &no_size).unwrap().is_empty());
    }

    #[test]
    fn unbounded_plan_is_capped_not_allocated() {
        // No max_parts + a tiny part size against a huge size would otherwise try
        // to allocate ~size parts; the absolute cap rejects it instead.
        let err = plan_parts(u64::MAX, &plan(1, 1, 1, None)).unwrap_err();
        assert!(matches!(err, StorageError::PolicyRejected { .. }));
    }
}
