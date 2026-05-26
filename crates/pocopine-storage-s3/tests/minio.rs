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
use aws_sdk_s3::Client;
use bytes::Bytes;
use pocopine_storage::{
    CompleteUpload, InitiateUpload, SafeObjectKey, StorageBackend, StorageContext, StorageError,
    StorageKey, StorageResult, UploadPolicy, UploadSession, UploadStrategy,
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
