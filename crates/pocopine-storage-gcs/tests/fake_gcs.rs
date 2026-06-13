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
    ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload, ObjectChecksum,
    PrincipalRef, SafeObjectKey, StorageActor, StorageBackend, StorageContext, StorageError,
    StorageKey, StorageResult, UploadBody, UploadPolicy, UploadSession, UploadSessionStatus,
    UploadStrategy,
};
use pocopine_storage_gcs::GcsStorageBackend;
use testcontainers::core::{ContainerPort, WaitFor, wait::HttpWaitStrategy};
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
                .with_wait_for(WaitFor::http(
                    HttpWaitStrategy::new("/storage/v1/b")
                        .with_port(ContainerPort::Tcp(FAKE_GCS_PORT))
                        .with_response_matcher(|response| response.status().as_u16() < 500),
                ))
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

fn session_object_key(backend: &GcsStorageBackend, session: &UploadSession, name: &str) -> String {
    format!(
        "{}/{}/{}",
        backend.internal_prefix(),
        session.id.as_str(),
        name
    )
}

fn ctx() -> StorageContext {
    StorageContext::system("gcs-fake-it")
}

fn principal_ctx(subject: &str) -> StorageContext {
    StorageContext {
        actor: StorageActor::Principal(PrincipalRef {
            subject: subject.to_string(),
            attributes: BTreeMap::new(),
        }),
        request: None,
    }
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

fn large_policy() -> StorageResult<UploadPolicy> {
    let mut policy = UploadPolicy::new("gcs")?
        .max_bytes(32 * 1024 * 1024)
        .preferred_chunk_size(1024 * 1024);
    policy.expires_after = Duration::from_secs(60 * 60);
    Ok(policy)
}

async fn initiate_large_checked(
    backend: &GcsStorageBackend,
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
                file_name: "large.bin".to_string(),
                size: Some(size),
                content_type: Some("application/octet-stream".to_string()),
                metadata: BTreeMap::new(),
                requested_strategy: UploadStrategy::Auto,
                policy,
            },
        )
        .await
}

fn pattern_bytes(start: usize, len: usize) -> Bytes {
    let mut buf = Vec::with_capacity(len);
    for i in 0..len {
        buf.push(((start + i) % 251) as u8);
    }
    Bytes::from(buf)
}

async fn object_exists(storage: &Storage, bucket: &str, key: &str) -> bool {
    storage
        .read_object(bucket_resource(bucket), key)
        .send()
        .await
        .is_ok()
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
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await;
    assert!(matches!(rejected, Err(StorageError::PolicyRejected { .. })));
    let inspected = backend.inspect_upload(&ctx(), session.id.clone()).await?;
    assert_eq!(inspected.status, UploadSessionStatus::Open);

    let repeated = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await;
    assert!(matches!(repeated, Err(StorageError::PolicyRejected { .. })));
    assert_eq!(
        object_bytes(&clients.storage, backend.bucket(), "files/hello.txt").await,
        b"existing"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_ignores_rogue_staged_object_and_advances_metadata() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "retry").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let session = initiate(&backend, Some(5)).await?;
    // Native compose buffers the tail inline in the session metadata; a stray
    // `bytes.tmp` object is not part of the protocol, so the append must write
    // its bytes as usual and advance the offset to the appended length without
    // ever consulting the rogue object.
    let bytes_key = session_object_key(&backend, &session, "bytes.tmp");
    clients
        .storage
        .write_object(
            backend.bucket_resource(),
            bytes_key,
            Bytes::from_static(b"hello"),
        )
        .send_unbuffered()
        .await
        .expect("seed rogue staged bytes");

    let updated = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;
    assert_eq!(updated.next_offset, Some(5));
    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.next_offset, Some(5));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rogue_staged_object_does_not_promote_committed_offset() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "truncate").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let session = initiate(&backend, Some(5)).await?;
    // Native compose sessions buffer their tail inline in the metadata; the
    // legacy `bytes.tmp` staging object is not part of the protocol, so a rogue
    // one must never be mistaken for committed bytes.
    let bytes_key = session_object_key(&backend, &session, "bytes.tmp");
    clients
        .storage
        .write_object(
            backend.bucket_resource(),
            bytes_key.clone(),
            Bytes::from_static(b"hello"),
        )
        .send_unbuffered()
        .await
        .expect("seed rogue staged bytes");

    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.next_offset, Some(0));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_upload_length_does_not_promote_staged_bytes() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "set-length").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let session = initiate(&backend, None).await?;
    let bytes_key = session_object_key(&backend, &session, "bytes.tmp");
    clients
        .storage
        .write_object(
            backend.bucket_resource(),
            bytes_key,
            Bytes::from_static(b"hello"),
        )
        .send_unbuffered()
        .await
        .expect("seed rogue staged bytes");

    let updated = backend
        .set_upload_length(&ctx(), session.id.clone(), 5)
        .await?;
    assert_eq!(updated.size, Some(5));
    assert_eq!(updated.next_offset, Some(0));
    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.next_offset, Some(0));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_system_actor_cannot_abort_corrupt_upload_metadata_against_fake_gcs()
-> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "corrupt-owner").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let session = initiate(&backend, Some(5)).await?;
    let meta_key = session_object_key(&backend, &session, "session.json");
    clients
        .storage
        .write_object(
            backend.bucket_resource(),
            meta_key.clone(),
            Bytes::from_static(b"{not-json"),
        )
        .send_unbuffered()
        .await
        .expect("corrupt session metadata");

    let rejected = backend
        .abort_upload(&principal_ctx("tenant-b"), session.id.clone())
        .await;
    assert!(matches!(rejected, Err(StorageError::Forbidden { .. })));
    assert_eq!(
        object_bytes(&clients.storage, backend.bucket(), &meta_key).await,
        b"{not-json"
    );

    backend.abort_upload(&ctx(), session.id).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_removes_corrupt_upload_metadata_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "corrupt").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let session = initiate(&backend, Some(5)).await?;
    let meta_key = session_object_key(&backend, &session, "session.json");
    clients
        .storage
        .write_object(
            backend.bucket_resource(),
            meta_key,
            Bytes::from_static(b"{not-json"),
        )
        .send_unbuffered()
        .await
        .expect("corrupt session metadata");

    backend.abort_upload(&ctx(), session.id.clone()).await?;
    let inspected = backend.inspect_upload(&ctx(), session.id).await;
    assert!(matches!(
        inspected,
        Err(StorageError::UnknownUploadSession { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_refuses_in_flight_completion_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "completing").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let session = initiate(&backend, Some(5)).await?;
    let meta_key = session_object_key(&backend, &session, "session.json");
    let mut session_json: serde_json::Value =
        serde_json::from_slice(&object_bytes(&clients.storage, backend.bucket(), &meta_key).await)
            .expect("decode session json");
    session_json["public"]["status"] = serde_json::json!("completing");
    session_json["completion_object_key"] = serde_json::json!("files/hello.txt");
    clients
        .storage
        .write_object(
            backend.bucket_resource(),
            meta_key,
            Bytes::from(serde_json::to_vec(&session_json).expect("encode completing session json")),
        )
        .send_unbuffered()
        .await
        .expect("write completing session metadata");

    let rejected = backend.abort_upload(&ctx(), session.id).await;
    assert!(matches!(rejected, Err(StorageError::UploadClosed { .. })));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initiate_rejects_policy_above_proxy_cap_for_streaming_upload() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "policy-cap").await;
    let backend = GcsStorageBackend::emulator(clients.storage, clients.endpoint, bucket)?
        .with_max_proxy_upload_bytes(8)?;
    let mut policy = policy()?.max_bytes(9);
    policy.preferred_chunk_size = Some(4);

    let rejected = backend
        .initiate_upload(
            &ctx(),
            InitiateUpload {
                scope: "files".to_string(),
                storage_key: StorageKey::new(SafeObjectKey::parse("files/hello.txt")?),
                file_name: "hello.txt".to_string(),
                size: None,
                content_type: Some("text/plain".to_string()),
                metadata: BTreeMap::new(),
                requested_strategy: UploadStrategy::Auto,
                policy,
            },
        )
        .await;
    assert!(matches!(
        rejected,
        Err(StorageError::PayloadTooLarge { limit: 8 })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_upload_assembles_large_object_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "compose").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;

    // 4 MiB in 2 MiB chunks; with a 1 MiB component size this writes several
    // component objects that are composed into the final object.
    let chunk = 2 * 1024 * 1024usize;
    let total = 2 * chunk;
    let session = initiate_large_checked(
        &backend,
        "files/large.bin",
        total as u64,
        ChecksumPolicy::None,
    )
    .await?;

    let mut offset = 0u64;
    for index in 0..2 {
        let updated = backend
            .append_upload_bytes(
                &ctx(),
                session.id.clone(),
                offset,
                pattern_bytes(index * chunk, chunk),
            )
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
    assert_eq!(
        object_bytes(&clients.storage, backend.bucket(), "files/large.bin").await,
        pattern_bytes(0, total).to_vec()
    );
    // Component objects are temporary and removed after completion.
    let first_component = session_object_key(&backend, &session, "components/comp-00000");
    assert!(!object_exists(&clients.storage, backend.bucket(), &first_component).await);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_abort_removes_component_objects_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "compose-abort").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let session = initiate_large_checked(
        &backend,
        "files/large.bin",
        2 * 1024 * 1024,
        ChecksumPolicy::None,
    )
    .await?;

    // 2 MiB at a 1 MiB component size writes component objects.
    backend
        .append_upload_bytes(
            &ctx(),
            session.id.clone(),
            0,
            pattern_bytes(0, 2 * 1024 * 1024),
        )
        .await?;
    let first_component = session_object_key(&backend, &session, "components/comp-00000");
    assert!(object_exists(&clients.storage, backend.bucket(), &first_component).await);

    backend.abort_upload(&ctx(), session.id.clone()).await?;

    assert!(!object_exists(&clients.storage, backend.bucket(), &first_component).await);
    assert!(!object_exists(&clients.storage, backend.bucket(), "files/large.bin").await);
    let inspected = backend.inspect_upload(&ctx(), session.id).await;
    assert!(matches!(
        inspected,
        Err(StorageError::UnknownUploadSession { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_completion_is_idempotent_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "compose-idem").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let total = 3 * 1024 * 1024usize;
    let session = initiate_large_checked(
        &backend,
        "files/large.bin",
        total as u64,
        ChecksumPolicy::None,
    )
    .await?;
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
async fn compose_upload_verifies_streaming_sha256_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "compose-sha").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    let total = 3 * 1024 * 1024usize;
    let data = pattern_bytes(0, total);
    let expected = ObjectChecksum {
        algorithm: ChecksumAlgorithm::Sha256,
        value: pocopine_crypto::sha256_hex(&data),
    };
    let session = initiate_large_checked(
        &backend,
        "files/checked.bin",
        total as u64,
        ChecksumPolicy::Required(ChecksumAlgorithm::Sha256),
    )
    .await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, data)
        .await?;

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
async fn compose_buffers_large_inline_tail_in_metadata_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "compose-tail").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;
    // 500 KiB stays below the 1 MiB component size, so it is buffered entirely
    // in the inline (base64) tail — well over the 256 KiB non-tail metadata
    // bound. inspect + complete must still read the session metadata.
    let total = 500 * 1024usize;
    let session = initiate_large_checked(
        &backend,
        "files/tail.bin",
        total as u64,
        ChecksumPolicy::None,
    )
    .await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, pattern_bytes(0, total))
        .await?;

    let inspected = backend.inspect_upload(&ctx(), session.id.clone()).await?;
    assert_eq!(inspected.next_offset, Some(total as u64));

    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.size, total as u64);
    assert_eq!(
        object_bytes(&clients.storage, backend.bucket(), "files/tail.bin").await,
        pattern_bytes(0, total).to_vec()
    );
    Ok(())
}

/// Initiate a server-mediated by-number multipart upload (the `upload_part`
/// path). Component size for the 32 MiB cap is 1 MiB, so parts are <= 1 MiB.
async fn initiate_multipart(
    backend: &GcsStorageBackend,
    key: &str,
    size: u64,
) -> StorageResult<UploadSession> {
    let mut policy = large_policy()?;
    policy.max_concurrent_parts = 4;
    backend
        .initiate_upload(
            &ctx(),
            InitiateUpload {
                scope: "files".to_string(),
                storage_key: StorageKey::new(SafeObjectKey::parse(key)?),
                file_name: "bynum.bin".to_string(),
                size: Some(size),
                content_type: Some("application/octet-stream".to_string()),
                metadata: BTreeMap::new(),
                requested_strategy: UploadStrategy::Multipart,
                policy,
            },
        )
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multipart_by_number_composes_parts_concurrently_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "bynum").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;

    // 1 MiB component size: two full parts plus a 512 KiB final part.
    let part = 1024 * 1024usize;
    let last = 512 * 1024usize;
    let total = (2 * part + last) as u64;
    let session = initiate_multipart(&backend, "files/bynum.bin", total).await?;
    assert_eq!(session.strategy, UploadStrategy::Multipart);
    assert!(session.plan.max_concurrent_parts >= 2);

    let specs = [
        (1u32, 0usize, part),
        (2u32, part, part),
        (3u32, 2 * part, last),
    ];
    let mut handles = Vec::new();
    for (number, start, len) in specs.into_iter().rev() {
        let backend = backend.clone();
        let session_id = session.id.clone();
        let bytes = pattern_bytes(start, len);
        handles.push(tokio::spawn(async move {
            backend
                .upload_part(&ctx(), session_id, number, UploadBody::from_bytes(bytes))
                .await
        }));
    }
    for handle in handles {
        handle.await.expect("join part task")?;
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
    assert_eq!(object.size, total);
    assert_eq!(
        object_bytes(&clients.storage, backend.bucket(), "files/bynum.bin").await,
        pattern_bytes(0, total as usize).to_vec()
    );

    // Idempotent re-complete returns the same object.
    let again = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(again, object);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multipart_by_number_rejects_missing_part_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "bynum-gap").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;

    let part = 1024 * 1024usize;
    let total = (3 * part) as u64;
    let session = initiate_multipart(&backend, "files/gap.bin", total).await?;
    // Upload parts 1 and 3, leaving a gap at 2.
    backend
        .upload_part(
            &ctx(),
            session.id.clone(),
            1,
            UploadBody::from_bytes(pattern_bytes(0, part)),
        )
        .await?;
    backend
        .upload_part(
            &ctx(),
            session.id.clone(),
            3,
            UploadBody::from_bytes(pattern_bytes(2 * part, part)),
        )
        .await?;

    let rejected = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await;
    assert!(
        matches!(rejected, Err(StorageError::PolicyRejected { .. })),
        "a missing part 2 must be rejected, got {rejected:?}"
    );
    assert!(!object_exists(&clients.storage, backend.bucket(), "files/gap.bin").await);

    // The session stays Open; uploading the missing part lets it complete.
    backend
        .upload_part(
            &ctx(),
            session.id.clone(),
            2,
            UploadBody::from_bytes(pattern_bytes(part, part)),
        )
        .await?;
    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.size, total);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multipart_by_number_rejects_wrong_part_length_against_fake_gcs() -> StorageResult<()> {
    let clients = gcs_clients().await;
    let bucket = create_bucket(&clients, "bynum-len").await;
    let backend =
        GcsStorageBackend::emulator(clients.storage.clone(), clients.endpoint.clone(), bucket)?;

    // 1 MiB component size, declared 2 MiB => part 1 must be exactly 1 MiB.
    let part = 1024 * 1024usize;
    let session = initiate_multipart(&backend, "files/len.bin", (2 * part) as u64).await?;
    let rejected = backend
        .upload_part(
            &ctx(),
            session.id.clone(),
            1,
            UploadBody::from_bytes(pattern_bytes(0, part / 2)),
        )
        .await;
    assert!(
        matches!(rejected, Err(StorageError::PolicyRejected { .. })),
        "an undersized non-final part must be rejected, got {rejected:?}"
    );
    Ok(())
}
