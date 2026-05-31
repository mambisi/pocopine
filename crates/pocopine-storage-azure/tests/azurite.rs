// Azurite-backed integration tests for `pocopine-storage-azure`.
//
// Requires a working Docker daemon. Local contributors without Docker can skip
// these with `cargo test -p pocopine-storage-azure --lib`.
//
// Host-only.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use azure_core::http::{NoFormat, RequestContent, Url};
use azure_storage_blob::BlobContainerClient;
use base64::Engine;
use bytes::Bytes;
use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use pocopine_storage::{
    ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload, ObjectChecksum,
    PrincipalRef, SafeObjectKey, StorageActor, StorageBackend, StorageContext, StorageError,
    StorageKey, StorageResult, UploadBody, UploadPolicy, UploadSession, UploadSessionStatus,
    UploadStrategy,
};
use pocopine_storage_azure::AzureBlobStorageBackend;
use sha2::Sha256;
use testcontainers::core::{wait::HttpWaitStrategy, ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;

const AZURITE_IMAGE: &str = "mcr.microsoft.com/azure-storage/azurite";
const AZURITE_TAG: &str = "3.35.0";
const AZURITE_BLOB_PORT: u16 = 10000;
const AZURITE_ACCOUNT: &str = "devstoreaccount1";
const AZURITE_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

static AZURITE: OnceCell<ContainerAsync<GenericImage>> = OnceCell::const_new();
static CONTAINER_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
struct AzuriteContainer {
    url: Url,
}

impl AzuriteContainer {
    fn client(&self) -> BlobContainerClient {
        BlobContainerClient::new(self.url.clone(), None, None)
            .expect("build Azurite container client")
    }
}

async fn azurite_endpoint() -> String {
    let container = AZURITE
        .get_or_init(|| async {
            GenericImage::new(AZURITE_IMAGE, AZURITE_TAG)
                .with_exposed_port(ContainerPort::Tcp(AZURITE_BLOB_PORT))
                .with_wait_for(WaitFor::http(
                    HttpWaitStrategy::new("/")
                        .with_port(ContainerPort::Tcp(AZURITE_BLOB_PORT))
                        .with_response_matcher(|response| response.status().as_u16() < 500),
                ))
                .with_cmd([
                    "azurite-blob",
                    "--blobHost",
                    "0.0.0.0",
                    "--blobPort",
                    "10000",
                    "--loose",
                    "--skipApiVersionCheck",
                ])
                .start()
                .await
                .expect("start Azurite testcontainer")
        })
        .await;
    let port = container
        .get_host_port_ipv4(AZURITE_BLOB_PORT)
        .await
        .expect("Azurite container port");
    format!("http://127.0.0.1:{port}/{AZURITE_ACCOUNT}")
}

async fn create_container(prefix: &str) -> AzuriteContainer {
    let endpoint = azurite_endpoint().await;
    let container = unique_container(prefix);
    create_container_with_shared_key(&endpoint, &container).await;
    let url = format!("{endpoint}/{container}?{}", service_sas_query(&container));
    AzuriteContainer {
        url: Url::parse(&url).expect("valid Azurite URL"),
    }
}

async fn create_container_with_shared_key(endpoint: &str, container: &str) {
    let date = "Tue, 26 May 2026 00:00:00 GMT";
    let version = "2020-12-06";
    let canonicalized_headers = format!("x-ms-date:{date}\nx-ms-version:{version}\n");
    let canonicalized_resource =
        format!("/{AZURITE_ACCOUNT}/{AZURITE_ACCOUNT}/{container}\nrestype:container");
    let string_to_sign =
        format!("PUT\n\n\n\n\n\n\n\n\n\n\n\n{canonicalized_headers}{canonicalized_resource}");
    let signature = hmac_sha256_base64(&string_to_sign);
    let response = reqwest::Client::new()
        .put(format!("{endpoint}/{container}?restype=container"))
        .header("x-ms-date", date)
        .header("x-ms-version", version)
        .header(
            "Authorization",
            format!("SharedKey {AZURITE_ACCOUNT}:{signature}"),
        )
        .send()
        .await
        .expect("create Azurite container request");
    let status = response.status();
    let body = response.text().await.expect("read create container body");
    assert!(
        status.is_success() || status.as_u16() == 409,
        "create Azurite container failed: {status} {body}"
    );
}

fn unique_container(prefix: &str) -> String {
    format!(
        "poco-{prefix}-{}-{}",
        std::process::id(),
        CONTAINER_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

fn service_sas_query(container: &str) -> String {
    let permissions = "racwdl";
    let start = "2024-01-01T00:00:00Z";
    let expiry = "2030-01-01T00:00:00Z";
    let protocol = "http";
    let version = "2020-12-06";
    let resource = "c";
    let canonicalized_resource = format!("/blob/{AZURITE_ACCOUNT}/{container}");
    let string_to_sign = [
        permissions,
        start,
        expiry,
        canonicalized_resource.as_str(),
        "",
        "",
        protocol,
        version,
        resource,
        "",
        "",
        "",
        "",
        "",
        "",
        "",
    ]
    .join("\n");
    let signature = hmac_sha256_base64(&string_to_sign);
    format!(
        "sv={}&st={}&se={}&sr={resource}&sp={permissions}&spr={protocol}&sig={}",
        encode_query_value(version),
        encode_query_value(start),
        encode_query_value(expiry),
        encode_query_value(&signature)
    )
}

fn encode_query_value(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn hmac_sha256_base64(string_to_sign: &str) -> String {
    let key = base64::engine::general_purpose::STANDARD
        .decode(AZURITE_KEY)
        .expect("decode Azurite account key");
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("create HMAC");
    mac.update(string_to_sign.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

fn session_object_key(
    backend: &AzureBlobStorageBackend,
    session: &UploadSession,
    name: &str,
) -> String {
    format!(
        "{}/{}/{}",
        backend.internal_prefix(),
        session.id.as_str(),
        name
    )
}

fn ctx() -> StorageContext {
    StorageContext::system("azure-azurite-it")
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
    let mut policy = UploadPolicy::new("azure")?
        .max_bytes(1024 * 1024)
        .preferred_chunk_size(4);
    policy.expires_after = Duration::from_secs(60 * 60);
    Ok(policy)
}

async fn initiate(
    backend: &AzureBlobStorageBackend,
    size: Option<u64>,
) -> StorageResult<UploadSession> {
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

async fn object_bytes(container: &AzuriteContainer, key: &str) -> Vec<u8> {
    container
        .client()
        .blob_client(key)
        .download(None)
        .await
        .expect("download blob")
        .body
        .collect()
        .await
        .expect("read blob body")
        .to_vec()
}

async fn put_object(container: &AzuriteContainer, key: impl AsRef<str>, bytes: &'static [u8]) {
    let content: RequestContent<Bytes, NoFormat> = Bytes::from_static(bytes).into();
    container
        .client()
        .blob_client(key.as_ref())
        .upload(content, None)
        .await
        .expect("put blob");
}

async fn blob_exists(container: &AzuriteContainer, key: &str) -> bool {
    container
        .client()
        .blob_client(key)
        .exists()
        .await
        .expect("check blob exists")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequential_upload_resumes_and_completes_against_azurite() -> StorageResult<()> {
    let container = create_container("resume").await;
    let backend = AzureBlobStorageBackend::new(container.client())?.with_prefix("tenant-a")?;
    let session = initiate(&backend, Some(11)).await?;

    let updated = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello "))
        .await?;
    assert_eq!(updated.next_offset, Some(6));

    let reloaded = AzureBlobStorageBackend::new(container.client())?.with_prefix("tenant-a")?;
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
    assert_eq!(object.backend, "azure");
    assert_eq!(object.scope, "files");
    assert_eq!(object.key, "files/hello.txt");
    assert_eq!(object.content_type.as_deref(), Some("text/plain"));
    assert_eq!(object.size, 11);
    assert_eq!(
        object.metadata.get("purpose").map(String::as_str),
        Some("integration")
    );
    assert_eq!(
        object_bytes(&container, "tenant-a/files/hello.txt").await,
        b"hello world"
    );

    let repeated = reloaded
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await?;
    assert_eq!(repeated, object);
    assert!(
        !blob_exists(
            &container,
            &session_object_key(&reloaded, &session, "bytes.tmp")
        )
        .await
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_offset_returns_typed_mismatch_against_azurite() -> StorageResult<()> {
    let container = create_container("offset").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
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
async fn unknown_session_and_repeated_abort_are_typed_against_azurite() -> StorageResult<()> {
    let container = create_container("unknown").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
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
    let container = create_container("length").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
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
    let container = create_container("collision").await;
    put_object(&container, "files/hello.txt", b"existing").await;

    let backend = AzureBlobStorageBackend::new(container.client())?;
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
        object_bytes(&container, "files/hello.txt").await,
        b"existing"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_append_retry_after_staged_write_advances_metadata() -> StorageResult<()> {
    let container = create_container("retry").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let session = initiate(&backend, Some(5)).await?;
    let bytes_key = session_object_key(&backend, &session, "bytes.tmp");
    put_object(&container, bytes_key, b"hello").await;

    let updated = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;
    assert_eq!(updated.next_offset, Some(5));
    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.next_offset, Some(5));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rogue_staged_object_does_not_corrupt_native_upload() -> StorageResult<()> {
    let container = create_container("truncate-append").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let session = initiate(&backend, Some(6)).await?;
    // Native block sessions stage blocks against the destination blob; the legacy
    // `bytes.tmp` staging object is not part of the protocol, so a rogue one must
    // not affect the committed result.
    let bytes_key = session_object_key(&backend, &session, "bytes.tmp");

    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"abc"))
        .await?;
    put_object(&container, bytes_key.clone(), b"abcjunk").await;
    let updated = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 3, Bytes::from_static(b"def"))
        .await?;
    assert_eq!(updated.next_offset, Some(6));

    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.size, 6);
    assert_eq!(object_bytes(&container, "files/hello.txt").await, b"abcdef");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspect_does_not_truncate_rogue_staged_bytes() -> StorageResult<()> {
    let container = create_container("truncate").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let session = initiate(&backend, Some(5)).await?;
    let bytes_key = session_object_key(&backend, &session, "bytes.tmp");
    put_object(&container, bytes_key.clone(), b"hello").await;

    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.next_offset, Some(0));
    assert_eq!(object_bytes(&container, &bytes_key).await, b"hello");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_upload_length_does_not_promote_staged_bytes() -> StorageResult<()> {
    let container = create_container("set-length").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let session = initiate(&backend, None).await?;
    let bytes_key = session_object_key(&backend, &session, "bytes.tmp");
    put_object(&container, bytes_key.clone(), b"hello").await;

    let updated = backend
        .set_upload_length(&ctx(), session.id.clone(), 5)
        .await?;
    assert_eq!(updated.size, Some(5));
    assert_eq!(updated.next_offset, Some(0));
    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.next_offset, Some(0));
    assert_eq!(object_bytes(&container, &bytes_key).await, b"hello");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_system_actor_cannot_abort_corrupt_upload_metadata() -> StorageResult<()> {
    let container = create_container("corrupt-owner").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let session = initiate(&backend, Some(5)).await?;
    let meta_key = session_object_key(&backend, &session, "session.json");
    put_object(&container, meta_key.clone(), b"{not-json").await;

    let rejected = backend
        .abort_upload(&principal_ctx("tenant-b"), session.id.clone())
        .await;
    assert!(matches!(rejected, Err(StorageError::Forbidden { .. })));
    // The refused abort left the (corrupt) session metadata in place. Native
    // sessions have no separate `bytes.tmp` staging object to check.
    assert!(blob_exists(&container, &meta_key).await);

    backend.abort_upload(&ctx(), session.id).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_removes_corrupt_upload_metadata_against_azurite() -> StorageResult<()> {
    let container = create_container("corrupt").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let session = initiate(&backend, Some(5)).await?;
    let meta_key = session_object_key(&backend, &session, "session.json");
    put_object(&container, meta_key, b"{not-json").await;

    backend.abort_upload(&ctx(), session.id.clone()).await?;
    let inspected = backend.inspect_upload(&ctx(), session.id).await;
    assert!(matches!(
        inspected,
        Err(StorageError::UnknownUploadSession { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_refuses_in_flight_completion_against_azurite() -> StorageResult<()> {
    let container = create_container("completing").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let session = initiate(&backend, Some(5)).await?;
    let meta_key = session_object_key(&backend, &session, "session.json");
    let mut session_json: serde_json::Value =
        serde_json::from_slice(&object_bytes(&container, &meta_key).await)
            .expect("decode session json");
    session_json["public"]["status"] = serde_json::json!("completing");
    session_json["completion_object_key"] = serde_json::json!("files/hello.txt");
    let bytes = serde_json::to_vec(&session_json).expect("encode completing session json");
    container
        .client()
        .blob_client(&meta_key)
        .upload(RequestContent::<Bytes, NoFormat>::from(bytes), None)
        .await
        .expect("write completing session metadata");

    let rejected = backend.abort_upload(&ctx(), session.id).await;
    assert!(matches!(rejected, Err(StorageError::UploadClosed { .. })));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initiate_rejects_policy_above_proxy_cap_for_streaming_upload() -> StorageResult<()> {
    let container = create_container("policy-cap").await;
    let backend =
        AzureBlobStorageBackend::new(container.client())?.with_max_proxy_upload_bytes(8)?;
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

fn large_policy() -> StorageResult<UploadPolicy> {
    let mut policy = UploadPolicy::new("azure")?
        .max_bytes(32 * 1024 * 1024)
        .preferred_chunk_size(1024 * 1024);
    policy.expires_after = Duration::from_secs(60 * 60);
    Ok(policy)
}

async fn initiate_large_checked(
    backend: &AzureBlobStorageBackend,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_upload_assembles_multiple_blocks_against_azurite() -> StorageResult<()> {
    let container = create_container("blocks").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    let chunk = 1024 * 1024usize;
    let total = 4 * chunk;
    let session = initiate_large_checked(
        &backend,
        "files/large.bin",
        total as u64,
        ChecksumPolicy::None,
    )
    .await?;

    let mut offset = 0u64;
    for index in 0..4 {
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
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.size, total as u64);
    assert_eq!(
        object_bytes(&container, "files/large.bin").await,
        pattern_bytes(0, total).to_vec()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_completion_is_idempotent_against_azurite() -> StorageResult<()> {
    let container = create_container("block-idem").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
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
async fn block_upload_verifies_streaming_sha256_against_azurite() -> StorageResult<()> {
    let container = create_container("block-sha").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
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
async fn block_complete_does_not_overwrite_existing_object_key() -> StorageResult<()> {
    let container = create_container("block-collision").await;
    put_object(&container, "files/large.bin", b"existing").await;

    let backend = AzureBlobStorageBackend::new(container.client())?;
    let total = 2 * 1024 * 1024usize;
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
        object_bytes(&container, "files/large.bin").await,
        b"existing"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_buffers_large_inline_tail_in_metadata_against_azurite() -> StorageResult<()> {
    let container = create_container("block-tail").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;
    // 500 KiB stays below the 1 MiB block size, so it is buffered entirely in the
    // inline (base64) tail — over the 256 KiB non-tail metadata bound. inspect +
    // complete must still read the session metadata.
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
        object_bytes(&container, "files/tail.bin").await,
        pattern_bytes(0, total).to_vec()
    );
    Ok(())
}

/// Initiate a server-mediated by-number multipart upload (the `upload_part`
/// path). Block size for the 32 MiB cap is 1 MiB, so parts are <= 1 MiB.
async fn initiate_multipart(
    backend: &AzureBlobStorageBackend,
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
async fn multipart_by_number_commits_blocks_concurrently_against_azurite() -> StorageResult<()> {
    let container = create_container("bynum").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;

    // 1 MiB block size: two full parts plus a 512 KiB final part.
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
        object_bytes(&container, "files/bynum.bin").await,
        pattern_bytes(0, total as usize).to_vec()
    );

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
async fn multipart_by_number_rejects_missing_block_against_azurite() -> StorageResult<()> {
    let container = create_container("bynum-gap").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;

    let part = 1024 * 1024usize;
    let total = (3 * part) as u64;
    let session = initiate_multipart(&backend, "files/gap.bin", total).await?;
    // Stage parts 1 and 3, leaving a gap at 2.
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
        "a missing block 2 must be rejected, got {rejected:?}"
    );
    assert!(!blob_exists(&container, "files/gap.bin").await);

    // The session stays Open; staging the missing part lets it complete.
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
    assert_eq!(
        object_bytes(&container, "files/gap.bin").await,
        pattern_bytes(0, total as usize).to_vec()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multipart_by_number_rejects_wrong_part_length_against_azurite() -> StorageResult<()> {
    let container = create_container("bynum-len").await;
    let backend = AzureBlobStorageBackend::new(container.client())?;

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
