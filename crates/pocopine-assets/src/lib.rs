//! RFC-100 — the asset-bucket client shared by the CLI sync and the
//! Mode B serving proxy.
//!
//! Pocopine's asset pipeline has exactly **one** bucket layout —
//! content-addressed keys `assets/<hash8>/<path>` written with an
//! immutable cache-control header — and this crate is the single
//! client for it. `pocopine assets push` (pocopine-cli) uses
//! [`AssetStore::list_keys`] + [`AssetStore::put`] for the hash-diff
//! sync; the `pocopine-server` `/assets/*` proxy uses
//! [`AssetStore::get`] to serve objects out of a private bucket.
//! Keeping both halves on this one type is what guarantees the
//! write path and the read path can never disagree about keys,
//! content types, or cache headers.
//!
//! The store speaks to any S3-compatible endpoint (Railway Buckets,
//! Cloudflare R2, AWS S3, MinIO) with static credentials; custom
//! endpoints are addressed path-style, which is what R2/MinIO/Railway
//! expect.
//!
//! This is deliberately a **leaf crate** (aws-sdk-s3 + tracing only):
//! `pocopine-server` consumes it, and the storage crates sit above
//! `pocopine-server` in the dependency graph, so the asset client
//! cannot live in `pocopine-storage-s3` without a cycle.

use aws_sdk_s3::Client;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::types::{BucketLocationConstraint, CreateBucketConfiguration};

/// Error from an [`AssetStore`] operation: a contextual message
/// wrapping the underlying SDK failure. Deliberately stringly — the
/// two consumers (CLI sync, server proxy) only log/relay it; nothing
/// branches on a failure kind.
#[derive(Debug)]
pub struct AssetStoreError(String);

impl AssetStoreError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for AssetStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AssetStoreError {}

/// Result alias for [`AssetStore`] operations.
pub type AssetResult<T> = Result<T, AssetStoreError>;

/// Cache header written on every asset object and echoed by every
/// serve path (dev server, Mode B proxy, public bucket metadata).
/// Asset keys embed a content hash, so the bytes under a key never
/// change — a year of `immutable` is safe by construction.
pub const ASSET_CACHE_CONTROL: &str = "public,max-age=31536000,immutable";

/// Key prefix under which all content-addressed assets live:
/// `assets/<hash8>/<relative-path>`.
pub const ASSET_KEY_PREFIX: &str = "assets/";

/// True for an 8-char lowercase-hex hash segment — the canonical
/// asset-hash shape, a prefix of [`pocopine_crypto::sha256_hex`] (see
/// `pocopine_core::assets::asset_hash` and the CLI/build hashers, which
/// all truncate to 8). Every serve path that recognises a hashed key or
/// bundle name routes through this one predicate so they cannot drift.
pub fn is_asset_hash(segment: &str) -> bool {
    segment.len() == 8
        && segment
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// True for the content-hashed bundle pair `pocopine build` emits: the
/// file name is `<stem>.<hash8>.js` or `<stem>.<hash8>.wasm`. Only the
/// bundle-pair extensions count, so a user file whose name happens to
/// contain a hex-looking segment doesn't silently become immutable.
/// Callers that hold a URL path should pass only the last segment.
pub fn is_hashed_bundle_name(file_name: &str) -> bool {
    let Some((stem, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    if ext != "js" && ext != "wasm" {
        return false;
    }
    stem.rsplit_once('.')
        .is_some_and(|(_, hash)| is_asset_hash(hash))
}

/// Connection settings for an [`AssetStore`].
///
/// The CLI fills this from `[package.metadata.pocopine.assets]` +
/// the assets entry in `~/.pocopine/credentials.toml`; the server
/// proxy fills it from `POCOPINE_ASSETS_*` env vars.
#[derive(Debug, Clone)]
pub struct AssetStoreConfig {
    /// S3-compatible endpoint URL. `None` means AWS S3 itself (the
    /// SDK derives the endpoint from `region`); `Some` switches the
    /// client to path-style addressing for compatibility with
    /// Railway Buckets / R2 / MinIO.
    pub endpoint: Option<String>,
    /// Bucket holding the `assets/<hash8>/<path>` keys.
    pub bucket: String,
    /// Signing region. `None` defaults to `us-east-1`, which all
    /// supported S3-compatible stores accept.
    pub region: Option<String>,
    /// Static access key id.
    pub access_key_id: String,
    /// Static secret access key.
    pub secret_access_key: String,
}

/// A fetched asset object — the Mode B proxy response payload.
///
/// v1 collects the full body in memory (the storage layer does not
/// yet expose a public streaming/Range read); follow-up tracked in
/// RFC-100 §10.
#[derive(Debug, Clone)]
pub struct FetchedAsset {
    /// Object bytes.
    pub bytes: Vec<u8>,
    /// Content type recorded at sync time, when the store returned
    /// one.
    pub content_type: Option<String>,
    /// Cache-control header recorded at sync time, when the store
    /// returned one. Serve paths fall back to
    /// [`ASSET_CACHE_CONTROL`] when absent.
    pub cache_control: Option<String>,
}

/// Client for the RFC-100 asset bucket. See the module docs for why
/// this is the single read/write path.
#[derive(Clone)]
pub struct AssetStore {
    client: Client,
    bucket: String,
    region: String,
    /// `true` when the bucket was addressed through a custom
    /// endpoint; controls the location constraint sent on
    /// [`Self::ensure_bucket`].
    custom_endpoint: bool,
}

impl std::fmt::Debug for AssetStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetStore")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl AssetStore {
    /// Build a store from connection settings. Pure construction —
    /// no network traffic until the first operation.
    pub fn connect(config: AssetStoreConfig) -> Self {
        let region = config
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());
        let credentials = Credentials::new(
            config.access_key_id.clone(),
            config.secret_access_key.clone(),
            None,
            None,
            "pocopine-assets",
        );
        let mut builder = aws_sdk_s3::Config::builder()
            .behavior_version_latest()
            .region(Region::new(region.clone()))
            .credentials_provider(credentials);
        let custom_endpoint = config.endpoint.is_some();
        if let Some(endpoint) = config.endpoint {
            // Custom endpoints (Railway, R2, MinIO) expect path-style
            // addressing; virtual-hosted style would prepend the
            // bucket to the host name and miss the service.
            builder = builder.endpoint_url(endpoint).force_path_style(true);
        }
        let client = Client::from_conf(builder.build());
        Self {
            client,
            bucket: config.bucket,
            region,
            custom_endpoint,
        }
    }

    /// Wrap an existing SDK client (used by tests that already built
    /// a MinIO-pointed client).
    pub fn from_client(client: Client, bucket: impl Into<String>) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            region: "us-east-1".to_string(),
            custom_endpoint: true,
        }
    }

    /// The configured bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Create the bucket when it does not exist yet; no-op when it
    /// does. Lets `pocopine assets push` work first-try against
    /// self-hosted MinIO / fresh dev buckets; on managed stores the
    /// bucket pre-exists and this reduces to one `HeadBucket`.
    pub async fn ensure_bucket(&self) -> AssetResult<()> {
        let head = self.client.head_bucket().bucket(&self.bucket).send().await;
        match head {
            Ok(_) => return Ok(()),
            Err(err) => {
                let not_found = err
                    .as_service_error()
                    .is_some_and(aws_sdk_s3::operation::head_bucket::HeadBucketError::is_not_found);
                if !not_found {
                    return Err(AssetStoreError::new(format!(
                        "head bucket `{}`: {}",
                        self.bucket,
                        aws_sdk_s3::error::DisplayErrorContext(&err)
                    )));
                }
            }
        }

        let mut create = self.client.create_bucket().bucket(&self.bucket);
        // AWS S3 outside us-east-1 requires an explicit location
        // constraint; custom-endpoint stores (MinIO/R2/Railway)
        // ignore or reject it, so only send it for real AWS.
        if !self.custom_endpoint && self.region != "us-east-1" {
            create = create.create_bucket_configuration(
                CreateBucketConfiguration::builder()
                    .location_constraint(BucketLocationConstraint::from(self.region.as_str()))
                    .build(),
            );
        }
        match create.send().await {
            Ok(_) => {
                tracing::info!(
                    target: "pocopine.log",
                    bucket = %self.bucket,
                    "created asset bucket"
                );
                Ok(())
            }
            Err(err) => {
                // Lost a creation race (or HeadBucket lacked
                // permission while the bucket is ours): both
                // "already owned by you" answers mean the bucket
                // exists, which is the goal state.
                let already = err.as_service_error().is_some_and(|e| {
                    e.is_bucket_already_owned_by_you() || e.is_bucket_already_exists()
                });
                if already {
                    return Ok(());
                }
                Err(AssetStoreError::new(format!(
                    "create bucket `{}`: {}",
                    self.bucket,
                    aws_sdk_s3::error::DisplayErrorContext(&err)
                )))
            }
        }
    }

    /// List every key under `prefix`, following continuation tokens.
    /// The sync diff feeds [`ASSET_KEY_PREFIX`] here and skips
    /// uploads whose content-addressed key already exists.
    pub async fn list_keys(&self, prefix: &str) -> AssetResult<std::collections::BTreeSet<String>> {
        let mut keys = std::collections::BTreeSet::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(token) = continuation.take() {
                request = request.continuation_token(token);
            }
            let page = request.send().await.map_err(|err| {
                AssetStoreError::new(format!(
                    "list `{prefix}` in bucket `{}`: {}",
                    self.bucket,
                    aws_sdk_s3::error::DisplayErrorContext(&err)
                ))
            })?;
            for object in page.contents() {
                if let Some(key) = object.key() {
                    keys.insert(key.to_string());
                }
            }
            match (page.is_truncated(), page.next_continuation_token()) {
                (Some(true), Some(token)) => continuation = Some(token.to_string()),
                _ => break,
            }
        }
        Ok(keys)
    }

    /// Write one asset object with its content type and the immutable
    /// cache header. Asset keys are content-addressed, so overwriting
    /// an existing key with the same bytes is harmless; the sync diff
    /// avoids it anyway to save the transfer.
    pub async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
        cache_control: &str,
    ) -> AssetResult<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .cache_control(cache_control)
            .body(bytes.into())
            .send()
            .await
            .map_err(|err| {
                AssetStoreError::new(format!(
                    "put `{key}` to bucket `{}`: {}",
                    self.bucket,
                    aws_sdk_s3::error::DisplayErrorContext(&err)
                ))
            })?;
        Ok(())
    }

    /// Fetch one asset object. `Ok(None)` when the key does not
    /// exist — the Mode B proxy turns that into a `404`.
    pub async fn get(&self, key: &str) -> AssetResult<Option<FetchedAsset>> {
        let response = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                if err
                    .as_service_error()
                    .is_some_and(aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key)
                {
                    return Ok(None);
                }
                return Err(AssetStoreError::new(format!(
                    "get `{key}` from bucket `{}`: {}",
                    self.bucket,
                    aws_sdk_s3::error::DisplayErrorContext(&err)
                )));
            }
        };
        let content_type = response.content_type().map(str::to_string);
        let cache_control = response.cache_control().map(str::to_string);
        let bytes = response.body.collect().await.map_err(|err| {
            AssetStoreError::new(format!(
                "read body of `{key}` from bucket `{}`: {err}",
                self.bucket
            ))
        })?;
        Ok(Some(FetchedAsset {
            bytes: bytes.into_bytes().to_vec(),
            content_type,
            cache_control,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_hash_shape() {
        assert!(is_asset_hash("b94d27b9"));
        assert!(!is_asset_hash("B94D27B9")); // uppercase
        assert!(!is_asset_hash("b94d27b")); // too short
        assert!(!is_asset_hash("b94d27b9a")); // too long
        assert!(!is_asset_hash("b94d27bg")); // non-hex
    }

    #[test]
    fn hashed_bundle_name_matches_pair_only() {
        assert!(is_hashed_bundle_name("website.0a1b2c3d.js"));
        assert!(is_hashed_bundle_name("website_bg.0a1b2c3d.wasm"));
        // Unhashed pair, wrong hash shape, other extensions.
        assert!(!is_hashed_bundle_name("website.js"));
        assert!(!is_hashed_bundle_name("website_bg.wasm"));
        assert!(!is_hashed_bundle_name("website.0A1B2C3D.js"));
        assert!(!is_hashed_bundle_name("website.abc.js"));
        assert!(!is_hashed_bundle_name("photo.0a1b2c3d.png"));
    }
}
