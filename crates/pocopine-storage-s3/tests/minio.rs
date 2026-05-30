// MinIO-backed integration tests for `pocopine-storage-s3`.
//
// Requires a working Docker daemon. Local contributors without Docker can skip
// these with `cargo test -p pocopine-storage-s3 --lib`.
//
// Host-only.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use bytes::Bytes;
use pocopine_storage::{
    ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload, ObjectChecksum,
    SafeObjectKey, StorageBackend, StorageContext, StorageError, StorageKey, StorageResult,
    UploadPolicy, UploadSession, UploadStrategy,
};
use pocopine_storage_s3::S3StorageBackend;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::minio::MinIO;
use tokio::sync::OnceCell;

const MINIO_PORT: u16 = 9000;
const MINIO_ROOT_USER: &str = "minioadmin";
const MINIO_ROOT_PASSWORD: &str = "minioadmin";

static MINIO: OnceCell<ContainerAsync<MinIO>> = OnceCell::const_new();
static BUCKET_COUNTER: AtomicUsize = AtomicUsize::new(0);

async fn minio_client() -> Client {
    let container = MINIO
        .get_or_init(|| async {
            MinIO::default()
                .start()
                .await
                .expect("start minio testcontainer")
        })
        .await;
    let port = container
        .get_host_port_ipv4(MINIO_PORT)
        .await
        .expect("minio container port");
    let endpoint = format!("http://127.0.0.1:{port}");
    s3_client(endpoint)
}

fn s3_client(endpoint: String) -> Client {
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint)
        .force_path_style(true)
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            MINIO_ROOT_USER,
            MINIO_ROOT_PASSWORD,
            None,
            None,
            "minio-test",
        ))
        .build();
    Client::from_conf(config)
}

async fn create_bucket(client: &Client, prefix: &str) -> String {
    let bucket = unique_bucket(prefix);
    client
        .create_bucket()
        .bucket(&bucket)
        .send()
        .await
        .expect("create minio bucket");
    bucket
}

fn unique_bucket(prefix: &str) -> String {
    format!(
        "pocopine-it-{prefix}-{}-{}",
        std::process::id(),
        BUCKET_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

fn ctx() -> StorageContext {
    StorageContext::system("s3-minio-it")
}

fn policy() -> StorageResult<UploadPolicy> {
    let mut policy = UploadPolicy::new("s3")?
        .max_bytes(1024 * 1024)
        .preferred_chunk_size(4);
    policy.expires_after = Duration::from_secs(60 * 60);
    Ok(policy)
}

async fn initiate(backend: &S3StorageBackend, size: Option<u64>) -> StorageResult<UploadSession> {
    backend
        .initiate_upload(
            &ctx(),
            InitiateUpload {
                scope: "files".to_string(),
                storage_key: StorageKey::new(SafeObjectKey::parse("files/hello.txt")?),
                file_name: "hello.txt".to_string(),
                size,
                content_type: Some("text/plain".to_string()),
                metadata: BTreeMap::from([("purpose".to_string(), "integration".to_string())]),
                requested_strategy: UploadStrategy::Auto,
                policy: policy()?,
            },
        )
        .await
}

/// Policy that allows large uploads so tests can cross the S3 5 MiB part floor.
fn large_policy() -> StorageResult<UploadPolicy> {
    let mut policy = UploadPolicy::new("s3")?
        .max_bytes(32 * 1024 * 1024)
        .preferred_chunk_size(1024 * 1024);
    policy.expires_after = Duration::from_secs(60 * 60);
    Ok(policy)
}

async fn initiate_large(
    backend: &S3StorageBackend,
    size: Option<u64>,
) -> StorageResult<UploadSession> {
    backend
        .initiate_upload(
            &ctx(),
            InitiateUpload {
                scope: "files".to_string(),
                storage_key: StorageKey::new(SafeObjectKey::parse("files/large.bin")?),
                file_name: "large.bin".to_string(),
                size,
                content_type: Some("application/octet-stream".to_string()),
                metadata: BTreeMap::new(),
                requested_strategy: UploadStrategy::Auto,
                policy: large_policy()?,
            },
        )
        .await
}

async fn initiate_checked(
    backend: &S3StorageBackend,
    key: &str,
    size: u64,
    checksum: ChecksumPolicy,
) -> StorageResult<UploadSession> {
    let mut policy = large_policy()?;
    policy.checksum = checksum;
    backend
        .initiate_upload(
            &ctx(),
            InitiateUpload {
                scope: "files".to_string(),
                storage_key: StorageKey::new(SafeObjectKey::parse(key)?),
                file_name: "checked.bin".to_string(),
                size: Some(size),
                content_type: Some("application/octet-stream".to_string()),
                metadata: BTreeMap::new(),
                requested_strategy: UploadStrategy::Auto,
                policy,
            },
        )
        .await
}

/// Deterministic, position-dependent bytes so assembled-object comparisons catch
/// any part ordering / offset bug.
fn pattern_bytes(start: usize, len: usize) -> Bytes {
    let mut buf = Vec::with_capacity(len);
    for i in 0..len {
        buf.push(((start + i) % 251) as u8);
    }
    Bytes::from(buf)
}

async fn object_exists(client: &Client, bucket: &str, key: &str) -> bool {
    client
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .is_ok()
}

async fn object_bytes(client: &Client, bucket: &str, key: &str) -> Vec<u8> {
    client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .expect("get completed object")
        .body
        .collect()
        .await
        .expect("read completed object")
        .into_bytes()
        .to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequential_upload_resumes_and_completes_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "resume").await;
    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?.with_prefix("tenant-a")?;
    let session = initiate(&backend, Some(11)).await?;

    let updated = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello "))
        .await?;
    assert_eq!(updated.next_offset, Some(6));

    let reloaded =
        S3StorageBackend::new(client.clone(), bucket.clone())?.with_prefix("tenant-a")?;
    let inspected = reloaded.inspect_upload(&ctx(), session.id.clone()).await?;
    assert_eq!(inspected.next_offset, Some(6));

    let updated = reloaded
        .append_upload_bytes(&ctx(), session.id.clone(), 6, Bytes::from_static(b"world"))
        .await?;
    assert_eq!(updated.next_offset, Some(11));

    let object = reloaded
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.backend, "s3");
    assert_eq!(object.scope, "files");
    assert_eq!(object.key, "files/hello.txt");
    assert_eq!(object.content_type.as_deref(), Some("text/plain"));
    assert_eq!(object.size, 11);
    assert_eq!(
        object.metadata.get("purpose").map(String::as_str),
        Some("integration")
    );
    assert_eq!(
        object_bytes(&client, &bucket, "tenant-a/files/hello.txt").await,
        b"hello world"
    );

    let repeated = reloaded
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(repeated, object);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_offset_returns_typed_mismatch_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "offset").await;
    let backend = S3StorageBackend::new(client, bucket)?;
    let session = initiate(&backend, Some(5)).await?;

    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hel"))
        .await?;

    let rejected = backend
        .append_upload_bytes(&ctx(), session.id, 1, Bytes::from_static(b"lo"))
        .await;
    assert!(matches!(
        rejected,
        Err(StorageError::OffsetMismatch {
            expected: 3,
            provided: 1
        })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_session_and_repeated_abort_are_typed_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "unknown").await;
    let backend = S3StorageBackend::new(client, bucket)?;
    let missing = pocopine_storage::UploadSessionId::new("missing-session")?;

    let inspected = backend.inspect_upload(&ctx(), missing.clone()).await;
    assert!(matches!(
        inspected,
        Err(StorageError::UnknownUploadSession { .. })
    ));

    backend.abort_upload(&ctx(), missing.clone()).await?;
    backend.abort_upload(&ctx(), missing).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_upload_length_uses_shared_invalid_value_error() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "length").await;
    let backend = S3StorageBackend::new(client, bucket)?;
    let session = initiate(&backend, None).await?;

    backend
        .set_upload_length(&ctx(), session.id.clone(), 5)
        .await?;
    let rejected = backend.set_upload_length(&ctx(), session.id, 6).await;
    assert!(matches!(
        rejected,
        Err(StorageError::InvalidValue { field, .. }) if field == "Upload-Length"
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_upload_assembles_large_object_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "multipart").await;
    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?;

    // 6 MiB sent as 3 x 2 MiB chunks. This crosses the 5 MiB part floor: the
    // first 5 MiB flush as one provider part, the trailing 1 MiB is the final
    // part written at completion.
    let chunk = 2 * 1024 * 1024usize;
    let total = 3 * chunk;
    let session = initiate_large(&backend, Some(total as u64)).await?;

    let mut offset = 0u64;
    for index in 0..3 {
        let bytes = pattern_bytes(index * chunk, chunk);
        let updated = backend
            .append_upload_bytes(&ctx(), session.id.clone(), offset, bytes)
            .await?;
        offset += chunk as u64;
        assert_eq!(updated.next_offset, Some(offset));
    }

    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.size, total as u64);
    // Bytes match exactly across the 5 MiB part boundary, proving the parts were
    // uploaded and assembled in order (not rewritten as one staged object).
    assert_eq!(
        object_bytes(&client, &bucket, "files/large.bin").await,
        pattern_bytes(0, total).to_vec()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_multipart_upload_removes_provider_parts_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "mp-abort").await;
    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?;
    let session = initiate_large(&backend, None).await?;

    // Push past the 5 MiB floor so a provider multipart upload actually exists.
    backend
        .append_upload_bytes(
            &ctx(),
            session.id.clone(),
            0,
            pattern_bytes(0, 6 * 1024 * 1024),
        )
        .await?;

    // Aborting issues `AbortMultipartUpload` (so this returns `Ok` only if the
    // provider accepted the abort) and drops local session state. (MinIO's
    // `ListMultipartUploads` does not report in-progress uploads, so cleanup is
    // verified via the observable effects below rather than by listing.)
    backend.abort_upload(&ctx(), session.id.clone()).await?;

    let inspected = backend.inspect_upload(&ctx(), session.id).await;
    assert!(matches!(
        inspected,
        Err(StorageError::UnknownUploadSession { .. })
    ));
    // The aborted upload never produced a final object.
    assert!(!object_exists(&client, &bucket, "files/large.bin").await);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_completion_is_idempotent_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "mp-idem").await;
    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?;
    let total = 6 * 1024 * 1024usize;
    let session = initiate_large(&backend, Some(total as u64)).await?;

    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, pattern_bytes(0, total))
        .await?;

    let first = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await?;
    let second = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(first, second);
    assert_eq!(first.size, total as u64);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_upload_verifies_streaming_sha256_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "mp-sha").await;
    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?;
    let total = 6 * 1024 * 1024usize;
    let data = pattern_bytes(0, total);
    let expected = ObjectChecksum {
        algorithm: ChecksumAlgorithm::Sha256,
        value: pocopine_crypto::sha256_hex(&data),
    };

    let session = initiate_checked(
        &backend,
        "files/checked.bin",
        total as u64,
        ChecksumPolicy::Required(ChecksumAlgorithm::Sha256),
    )
    .await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, data)
        .await?;

    // The checksum is verified by streaming the assembled object back through the
    // multipart path (no full in-memory buffer).
    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: Some(expected.clone()),
            },
        )
        .await?;
    assert_eq!(object.checksum, Some(expected));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_part_upload_verifies_crc32c_against_minio() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "sp-crc").await;
    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?;
    // Below the 5 MiB floor: completes via the single direct-PUT path.
    let data = pattern_bytes(0, 4096);
    let expected = ObjectChecksum {
        algorithm: ChecksumAlgorithm::Crc32c,
        value: pocopine_crypto::crc32c_hex(&data),
    };

    let session = initiate_checked(
        &backend,
        "files/crc.bin",
        data.len() as u64,
        ChecksumPolicy::Required(ChecksumAlgorithm::Crc32c),
    )
    .await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, data)
        .await?;

    let wrong = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: Some(ObjectChecksum {
                    algorithm: ChecksumAlgorithm::Crc32c,
                    value: "deadbeef".to_string(),
                }),
            },
        )
        .await;
    assert!(matches!(wrong, Err(StorageError::PolicyRejected { .. })));

    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: Some(expected.clone()),
            },
        )
        .await?;
    assert_eq!(object.checksum, Some(expected));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_does_not_overwrite_existing_object_key() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "collision").await;
    client
        .put_object()
        .bucket(&bucket)
        .key("files/hello.txt")
        .body(ByteStream::from_static(b"existing"))
        .send()
        .await
        .expect("seed existing object");

    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?;
    let session = initiate(&backend, Some(5)).await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;

    let rejected = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await;
    assert!(matches!(rejected, Err(StorageError::PolicyRejected { .. })));
    assert_eq!(
        object_bytes(&client, &bucket, "files/hello.txt").await,
        b"existing"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multipart_complete_does_not_overwrite_existing_object_key() -> StorageResult<()> {
    let client = minio_client().await;
    let bucket = create_bucket(&client, "mp-collision").await;
    client
        .put_object()
        .bucket(&bucket)
        .key("files/large.bin")
        .body(ByteStream::from_static(b"existing"))
        .send()
        .await
        .expect("seed existing object");

    let backend = S3StorageBackend::new(client.clone(), bucket.clone())?;
    let total = 6 * 1024 * 1024usize;
    let session = initiate_large(&backend, Some(total as u64)).await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, pattern_bytes(0, total))
        .await?;

    // The multipart completion path must also refuse to clobber an existing key
    // (CompleteMultipartUpload would otherwise overwrite it).
    let rejected = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await;
    assert!(matches!(rejected, Err(StorageError::PolicyRejected { .. })));
    assert_eq!(
        object_bytes(&client, &bucket, "files/large.bin").await,
        b"existing"
    );
    Ok(())
}
