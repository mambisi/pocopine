// fake-gcs-server-backed integration tests for `pocopine-storage-gcs`.
//
// Requires a working Docker daemon. Local contributors without Docker can skip
// these with `cargo test -p pocopine-storage-gcs --lib`.
//
// Host-only.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use google_cloud_auth::credentials::anonymous::Builder as Anonymous;
use google_cloud_storage::client::Storage;
use pocopine_storage::{
    CompleteUpload, InitiateUpload, SafeObjectKey, StorageBackend, StorageContext, StorageError,
    StorageKey, StorageResult, UploadPolicy, UploadSession, UploadStrategy,
};
use pocopine_storage_gcs::GcsStorageBackend;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;

const FAKE_GCS_IMAGE: &str = "fsouza/fake-gcs-server";
const FAKE_GCS_TAG: &str = "1.52.3";
const FAKE_GCS_PORT: u16 = 4443;

static FAKE_GCS: OnceCell<ContainerAsync<GenericImage>> = OnceCell::const_new();
static BUCKET_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
struct GcsClients {
    storage: Storage,
    endpoint: String,
    http: reqwest::Client,
}

async fn fake_gcs_endpoint() -> String {
    let container = FAKE_GCS
        .get_or_init(|| async {
            GenericImage::new(FAKE_GCS_IMAGE, FAKE_GCS_TAG)
                .with_exposed_port(ContainerPort::Tcp(FAKE_GCS_PORT))
                .with_wait_for(WaitFor::seconds(1))
                .with_cmd(["-scheme", "http", "-port", "4443", "-backend", "memory"])
                .start()
                .await
                .expect("start fake-gcs-server testcontainer")
        })
        .await;
    let port = container
        .get_host_port_ipv4(FAKE_GCS_PORT)
        .await
        .expect("fake-gcs-server container port");
    format!("http://127.0.0.1:{port}")
}

async fn gcs_clients() -> GcsClients {
    let endpoint = fake_gcs_endpoint().await;
    let credentials = Anonymous::new().build();
    let storage = Storage::builder()
        .with_endpoint(endpoint.clone())
        .with_credentials(credentials)
        .build()
        .await
        .expect("build fake gcs storage client");
    GcsClients {
        storage,
        endpoint,
        http: reqwest::Client::new(),
    }
}

async fn create_bucket(clients: &GcsClients, prefix: &str) -> String {
    let bucket = unique_bucket(prefix);
    let response = clients
        .http
        .post(format!("{}/storage/v1/b", clients.endpoint))
        .query(&[("project", "test")])
        .json(&serde_json::json!({ "name": bucket }))
        .send()
        .await
        .expect("create fake gcs bucket");
    let status = response.status();
    let body = response.text().await.expect("read create bucket body");
    assert!(status.is_success(), "create bucket failed: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).expect("decode created bucket");
    assert_eq!(created["name"].as_str(), Some(bucket.as_str()));
    bucket
}

fn unique_bucket(prefix: &str) -> String {
    format!(
        "pocopine-it-{prefix}-{}-{}",
        std::process::id(),
        BUCKET_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

fn bucket_resource(bucket: &str) -> String {
    format!("projects/_/buckets/{bucket}")
}

fn ctx() -> StorageContext {
    StorageContext::system("gcs-fake-it")
}

fn policy() -> StorageResult<UploadPolicy> {
    let mut policy = UploadPolicy::new("gcs")?
        .max_bytes(1024 * 1024)
        .preferred_chunk_size(4);
    policy.expires_after = Duration::from_secs(60 * 60);
    Ok(policy)
}

async fn initiate(backend: &GcsStorageBackend, size: Option<u64>) -> StorageResult<UploadSession> {
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

async fn object_bytes(storage: &Storage, bucket: &str, key: &str) -> Vec<u8> {
    let mut response = storage
        .read_object(bucket_resource(bucket), key)
        .send()
        .await
        .expect("read completed object");
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .next()
        .await
        .transpose()
        .expect("read object chunk")
    {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_storage_client_object_smoke_against_fake_gcs() {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "smoke").await;
    let bucket_resource = bucket_resource(&bucket);
    let prefix = "google-client-smoke";
    const CONTENTS: &str = "the quick brown fox jumps over the lazy dog";

    let inserted = clients
        .storage
        .write_object(
            &bucket_resource,
            format!("{prefix}/quick.txt"),
            Bytes::from_static(CONTENTS.as_bytes()),
        )
        .set_metadata([("verify-metadata-works", "yes")])
        .set_content_type("text/plain")
        .send_unbuffered()
        .await
        .expect("write object through google client");
    assert_eq!(inserted.name, format!("{prefix}/quick.txt"));
    assert_eq!(
        inserted
            .metadata
            .get("verify-metadata-works")
            .map(String::as_str),
        Some("yes")
    );

    let mut response = clients
        .storage
        .read_object(&bucket_resource, &inserted.name)
        .send()
        .await
        .expect("read object through google client");
    let object = response.object();
    assert!(
        object.content_type.starts_with("text/plain"),
        "unexpected content type: {}",
        object.content_type
    );

    let mut contents = Vec::new();
    while let Some(chunk) = response
        .next()
        .await
        .transpose()
        .expect("read object chunk")
    {
        contents.extend_from_slice(&chunk);
    }
    assert_eq!(contents, CONTENTS.as_bytes());

    let response = clients
        .http
        .get(format!("{}/storage/v1/b/{}/o", clients.endpoint, bucket))
        .query(&[("prefix", prefix)])
        .send()
        .await
        .expect("list objects through fake gcs json api");
    let status = response.status();
    let body = response.text().await.expect("read list objects body");
    assert!(status.is_success(), "list objects failed: {status} {body}");
    let listed: serde_json::Value = serde_json::from_str(&body).expect("decode listed objects");
    let mut names = BTreeSet::new();
    if let Some(items) = listed["items"].as_array() {
        for object in items {
            if let Some(name) = object["name"].as_str() {
                names.insert(name.to_string());
            }
        }
    }
    assert_eq!(names, BTreeSet::from([inserted.name]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequential_upload_resumes_and_completes_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "resume").await;
    let backend = GcsStorageBackend::emulator(
        clients.storage.clone(),
        clients.endpoint.clone(),
        bucket.clone(),
    )?
    .with_prefix("tenant-a")?;
    let session = initiate(&backend, Some(11)).await?;

    let updated = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello "))
        .await?;
    assert_eq!(updated.next_offset, Some(6));

    let reloaded = GcsStorageBackend::emulator(
        clients.storage.clone(),
        clients.endpoint.clone(),
        bucket.clone(),
    )?
    .with_prefix("tenant-a")?;
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
    assert_eq!(object.backend, "gcs");
    assert_eq!(object.scope, "files");
    assert_eq!(object.key, "files/hello.txt");
    assert!(object.version.is_some());
    assert_eq!(object.content_type.as_deref(), Some("text/plain"));
    assert_eq!(object.size, 11);
    assert_eq!(
        object.metadata.get("purpose").map(String::as_str),
        Some("integration")
    );
    assert_eq!(
        object_bytes(&clients.storage, &bucket, "tenant-a/files/hello.txt").await,
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
async fn wrong_offset_returns_typed_mismatch_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "offset").await;
    let backend = GcsStorageBackend::emulator(clients.storage, clients.endpoint, bucket)?;
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
async fn unknown_session_and_repeated_abort_are_typed_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "unknown").await;
    let backend = GcsStorageBackend::emulator(clients.storage, clients.endpoint, bucket)?;
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
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "length").await;
    let backend = GcsStorageBackend::emulator(clients.storage, clients.endpoint, bucket)?;
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
async fn complete_does_not_overwrite_existing_object_key() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "collision").await;
    let bucket_resource = bucket_resource(&bucket);
    clients
        .storage
        .write_object(
            &bucket_resource,
            "files/hello.txt",
            Bytes::from_static(b"existing"),
        )
        .send_unbuffered()
        .await
        .expect("seed existing object");

    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
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
        object_bytes(&clients.storage, backend.bucket(), "files/hello.txt").await,
        b"existing"
    );
    Ok(())
}
