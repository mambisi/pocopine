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
    if value.len() > MAX_STORAGE_TOKEN_LEN
        || trimmed.is_empty()
        || trimmed != value
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
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
        if let Some(max_parts) = self.max_parts {
            if max_parts == 0 {
                return Err(StorageError::policy_rejected(
                    "upload policy max_parts must be greater than zero",
                ));
            }
        }
        if matches!(
            self.checksum,
            ChecksumPolicy::Required(ChecksumAlgorithm::Crc32c | ChecksumAlgorithm::Md5)
        ) {
            return Err(StorageError::unsupported(
                "only sha256 checksum verification is implemented in pocopine-storage PR 1",
            ));
        }
        if let (Some(min), Some(preferred)) = (self.min_part_size, self.preferred_chunk_size) {
            if min > preferred {
                return Err(StorageError::policy_rejected(
                    "upload policy min_part_size cannot exceed preferred_chunk_size",
                ));
            }
        }
        if let (Some(preferred), Some(max)) = (self.preferred_chunk_size, self.max_part_size) {
            if preferred > max {
                return Err(StorageError::policy_rejected(
                    "upload policy preferred_chunk_size cannot exceed max_part_size",
                ));
            }
        }
        if let (Some(min), Some(max)) = (self.min_part_size, self.max_part_size) {
            if min > max {
                return Err(StorageError::policy_rejected(
                    "upload policy min_part_size cannot exceed max_part_size",
                ));
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn validate_initiate(&self, request: &InitiateUploadRequest) -> StorageResult<()> {
        if let Some(size) = request.size {
            if size > self.max_bytes {
                return Err(StorageError::payload_too_large(self.max_bytes));
            }
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
    pub(crate) fn descriptor(&self, scope: &str) -> UploadPolicyDescriptor {
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
            strategies: vec![UploadStrategy::Sequential],
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

/// Provider-neutral backend kind descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StorageBackendKind {
    Memory,
    LocalFs,
    S3Compatible,
    Gcs,
    AzureBlob,
    Custom(String),
}

pub(crate) fn file_extension(file_name: &str) -> Option<&str> {
    file_name
        .rsplit_once('.')
        .and_then(|(_, extension)| (!extension.is_empty()).then_some(extension))
}
