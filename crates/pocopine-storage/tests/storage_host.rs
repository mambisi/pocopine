#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::BodyExt;
use pocopine_core::ServerError;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_server::axum::Router;
use pocopine_storage::{
    storage_server_plugin, ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload,
    InitiateUploadRequest, LocalFsStorageBackend, MemoryStorageBackend, ObjectChecksum,
    ObjectMetadata, ObjectOwnerRef, SafeObjectKey, StorageBackend, StorageContext, StorageError,
    StorageKey, StorageKeyFuture, StorageKeyResolver, StorageResult, StorageScope, StorageServer,
    UploadIntent, UploadPolicy, UploadSession, UploadSessionId, UploadStrategy,
    STORAGE_ANON_COOKIE, STORAGE_UPLOADS_PATH,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tempfile::tempdir;
use tower::ServiceExt;

#[test]
fn safe_object_key_rejects_path_escape_and_reserved_prefixes() {
    assert!(SafeObjectKey::parse("avatars/user-1/photo.png").is_ok());

    for key in [
        "",
        "/absolute",
        "avatars//photo.png",
        "avatars/../photo.png",
        "avatars/./photo.png",
        "avatars\\photo.png",
        "__pocopine/session",
        ".pocopine-storage/session",
        "avatars/\nphoto.png",
        "avatars/CON",
        "avatars/photo?.png",
        "avatars/trailing-space ",
        "avatars/trailing-dot.",
        "avatars/c:photo.png",
    ] {
        assert!(
            SafeObjectKey::parse(key).is_err(),
            "key should be rejected: {key:?}"
        );
    }
}

#[tokio::test]
async fn route_rejects_unbound_anonymous_uploads() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let rejected: StorageResult<UploadSession> = post_json_without_cookie(
        router,
        STORAGE_UPLOADS_PATH,
        &initiate_request("avatars", UploadStrategy::Auto),
    )
    .await;

    assert!(matches!(rejected, Err(StorageError::Unauthorized { .. })));
    Ok(())
}

#[tokio::test]
async fn scope_registration_and_write_guard_run_before_key_resolution() -> StorageResult<()> {
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let resolver = CountingResolver {
        calls: resolver_calls.clone(),
    };
    let policy = policy("memory")?;
    let denied_scope = StorageScope::builder(policy.clone())
        .write_guard(|_ctx| async { Err(ServerError::forbidden("denied")) })
        .key_resolver(resolver)
        .build();
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .scope("avatars", denied_scope)?
        .build();

    let denied = storage
        .initiate_upload(ctx(), initiate_request("avatars", UploadStrategy::Auto))
        .await;
    assert!(matches!(denied, Err(StorageError::Forbidden { .. })));
    assert_eq!(
        resolver_calls.load(Ordering::SeqCst),
        0,
        "resolver must not run when the write guard denies the request"
    );

    let unknown = storage.descriptor(ctx(), "unknown").await;
    assert!(matches!(unknown, Err(StorageError::UnknownScope { .. })));
    Ok(())
}

#[tokio::test]
async fn route_rejects_patch_body_over_part_cap() -> StorageResult<()> {
    let mut policy = policy("memory")?;
    policy.max_part_size = Some(4);
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .public_scope("avatars", policy)?
        .build();
    let router = finalize(storage);
    let mut request = initiate_request("avatars", UploadStrategy::Auto);
    request.size = Some(8);
    let session: UploadSession = post_json(router.clone(), STORAGE_UPLOADS_PATH, &request).await?;

    let rejected: StorageResult<UploadSession> =
        patch_bytes_outer(router, &session.id, 0, "hello".as_bytes()).await;
    assert!(matches!(rejected, Err(StorageError::PolicyRejected { .. })));
    Ok(())
}

#[tokio::test]
async fn backends_reject_declared_size_and_policy_overflow() -> StorageResult<()> {
    let backend = MemoryStorageBackend::new();
    let session = initiate_direct(&backend).await?;
    let declared_overflow = backend
        .append_upload_bytes(&ctx(), session.id, 0, Bytes::from_static(b"hello!"))
        .await;
    assert!(matches!(
        declared_overflow,
        Err(StorageError::PolicyRejected { .. })
    ));

    let mut small_policy = policy(backend.name())?.max_bytes(5);
    small_policy.allowed_content_types.clear();
    small_policy.allowed_extensions.clear();
    let session = initiate_direct_with(&backend, None, small_policy).await?;
    let policy_overflow = backend
        .append_upload_bytes(&ctx(), session.id, 0, Bytes::from_static(b"hello!"))
        .await;
    assert!(matches!(
        policy_overflow,
        Err(StorageError::PolicyRejected { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn required_sha256_checksum_is_verified() -> StorageResult<()> {
    let backend = MemoryStorageBackend::new();
    let mut checksum_policy = policy(backend.name())?;
    checksum_policy.checksum = ChecksumPolicy::Required(ChecksumAlgorithm::Sha256);
    let session = initiate_direct_with(&backend, Some(5), checksum_policy).await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;

    let missing = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await;
    assert!(matches!(missing, Err(StorageError::PolicyRejected { .. })));

    let wrong = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: Some(ObjectChecksum {
                    algorithm: ChecksumAlgorithm::Sha256,
                    value: "00".to_string(),
                }),
            },
        )
        .await;
    assert!(matches!(wrong, Err(StorageError::PolicyRejected { .. })));

    let checksum = ObjectChecksum {
        algorithm: ChecksumAlgorithm::Sha256,
        value: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string(),
    };
    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: Some(checksum.clone()),
            },
        )
        .await?;
    assert_eq!(object.checksum, Some(checksum));
    Ok(())
}

#[tokio::test]
async fn abort_uses_delete_guard() -> StorageResult<()> {
    let scope = StorageScope::builder(policy("memory")?)
        .delete_guard(|_ctx| async { Err(ServerError::forbidden("delete denied")) })
        .build();
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .scope("avatars", scope)?
        .build();
    let router = finalize(storage);
    let session: UploadSession = post_json(
        router.clone(),
        STORAGE_UPLOADS_PATH,
        &initiate_request("avatars", UploadStrategy::Auto),
    )
    .await?;

    let rejected = delete_upload(router, &session.id).await;
    assert!(matches!(rejected, Err(StorageError::Forbidden { .. })));
    Ok(())
}

#[tokio::test]
async fn sequential_route_appends_bytes_and_reports_offset_mismatch() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let session: UploadSession = post_json(
        router.clone(),
        STORAGE_UPLOADS_PATH,
        &initiate_request("avatars", UploadStrategy::Auto),
    )
    .await?;

    let updated: UploadSession =
        patch_bytes(router.clone(), &session.id, 0, "hello".as_bytes()).await?;
    assert_eq!(updated.next_offset, Some(5));

    let mismatch: StorageResult<UploadSession> =
        patch_bytes_outer(router, &session.id, 0, "again".as_bytes()).await;
    assert!(matches!(
        mismatch,
        Err(StorageError::OffsetMismatch {
            expected: 5,
            provided: 0
        })
    ));
    Ok(())
}

#[tokio::test]
async fn local_fs_persists_session_and_resumes_after_backend_reload() -> StorageResult<()> {
    let tmp = tempdir().unwrap();
    let backend = LocalFsStorageBackend::new(tmp.path());
    let session = initiate_direct(&backend).await?;
    let meta = tmp
        .path()
        .join(".pocopine-storage")
        .join("sessions")
        .join(session.id.as_str())
        .join("session.json");
    assert!(
        meta.exists(),
        "local backend should persist upload metadata"
    );

    let updated = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;
    assert_eq!(updated.next_offset, Some(5));

    let reloaded = LocalFsStorageBackend::new(tmp.path());
    let inspected = reloaded.inspect_upload(&ctx(), session.id.clone()).await?;
    assert_eq!(inspected.next_offset, Some(5));
    Ok(())
}

#[tokio::test]
async fn local_fs_complete_is_stable_and_moves_temp_file_to_final_key() -> StorageResult<()> {
    let tmp = tempdir().unwrap();
    let backend = LocalFsStorageBackend::new(tmp.path());
    let session = initiate_direct(&backend).await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;

    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.key, "avatars/user-1/photo.txt");
    assert_eq!(object.size, 5);
    assert!(tmp.path().join("avatars/user-1/photo.txt").exists());
    assert!(
        !tmp.path()
            .join(".pocopine-storage")
            .join("sessions")
            .join(session.id.as_str())
            .join("bytes.tmp")
            .exists(),
        "complete should move temp bytes out of the session directory"
    );

    let repeated = backend
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

#[tokio::test]
async fn local_fs_abort_removes_session_files_and_is_idempotent() -> StorageResult<()> {
    let tmp = tempdir().unwrap();
    let backend = LocalFsStorageBackend::new(tmp.path());
    let session = initiate_direct(&backend).await?;
    let session_dir = tmp
        .path()
        .join(".pocopine-storage")
        .join("sessions")
        .join(session.id.as_str());
    assert!(session_dir.exists());

    backend.abort_upload(&ctx(), session.id.clone()).await?;
    assert!(!session_dir.exists());
    backend.abort_upload(&ctx(), session.id).await?;
    Ok(())
}

fn memory_storage() -> StorageResult<StorageServer> {
    Ok(StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .public_scope("avatars", policy("memory")?)?
        .build())
}

fn finalize(storage: StorageServer) -> Router {
    pocopine_server::__reset_for_test();
    pocopine_server::Server::new(Router::new())
        .plugin(storage_server_plugin(storage))
        .try_finalize()
        .unwrap()
}

async fn post_json<T, R>(router: Router, uri: &str, payload: &T) -> StorageResult<R>
where
    T: Serialize,
    R: DeserializeOwned,
{
    post_json_inner(router, uri, payload, true).await
}

async fn post_json_without_cookie<T, R>(router: Router, uri: &str, payload: &T) -> StorageResult<R>
where
    T: Serialize,
    R: DeserializeOwned,
{
    post_json_inner(router, uri, payload, false).await
}

async fn post_json_inner<T, R>(
    router: Router,
    uri: &str,
    payload: &T,
    include_cookie: bool,
) -> StorageResult<R>
where
    T: Serialize,
    R: DeserializeOwned,
{
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if include_cookie {
        builder = builder.header("cookie", anon_cookie());
    }
    let response = router
        .oneshot(
            builder
                .body(Body::from(serde_json::to_string(payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: StorageResult<R> = serde_json::from_slice(&bytes).unwrap();
    outer
}

async fn patch_bytes(
    router: Router,
    session: &UploadSessionId,
    offset: u64,
    bytes: &[u8],
) -> StorageResult<UploadSession> {
    patch_bytes_outer(router, session, offset, bytes).await
}

async fn patch_bytes_outer<R>(
    router: Router,
    session: &UploadSessionId,
    offset: u64,
    bytes: &[u8],
) -> StorageResult<R>
where
    R: DeserializeOwned,
{
    let response = router
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/__pocopine/storage/v1/uploads/{}/bytes",
                    session.as_str()
                ))
                .header("Upload-Offset", offset.to_string())
                .header("cookie", anon_cookie())
                .body(Body::from(bytes.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: StorageResult<R> = serde_json::from_slice(&bytes).unwrap();
    outer
}

async fn delete_upload(router: Router, session: &UploadSessionId) -> StorageResult<()> {
    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/__pocopine/storage/v1/uploads/{}",
                    session.as_str()
                ))
                .header("cookie", anon_cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn initiate_direct<B>(backend: &B) -> StorageResult<UploadSession>
where
    B: StorageBackend,
{
    initiate_direct_with(backend, Some(5), policy(backend.name())?).await
}

async fn initiate_direct_with<B>(
    backend: &B,
    size: Option<u64>,
    policy: UploadPolicy,
) -> StorageResult<UploadSession>
where
    B: StorageBackend,
{
    backend
        .initiate_upload(
            &ctx(),
            InitiateUpload {
                scope: "avatars".to_string(),
                storage_key: storage_key()?,
                file_name: "photo.txt".to_string(),
                size,
                content_type: Some("text/plain".to_string()),
                metadata: [("alt".to_string(), "Profile".to_string())].into(),
                requested_strategy: UploadStrategy::Auto,
                policy,
            },
        )
        .await
}

fn initiate_request(scope: &str, strategy: UploadStrategy) -> InitiateUploadRequest {
    InitiateUploadRequest {
        protocol: pocopine_storage::STORAGE_PROTOCOL_V1.to_string(),
        scope: scope.to_string(),
        file_name: "photo.txt".to_string(),
        size: Some(5),
        content_type: Some("text/plain".to_string()),
        metadata: Default::default(),
        requested_strategy: strategy,
    }
}

fn storage_key() -> StorageResult<StorageKey> {
    Ok(
        StorageKey::new(SafeObjectKey::parse("avatars/user-1/photo.txt")?)
            .owner(ObjectOwnerRef::principal("user-1"))
            .metadata(ObjectMetadata::from_iter([("kind", "avatar")])),
    )
}

fn policy(backend: &str) -> StorageResult<UploadPolicy> {
    Ok(UploadPolicy::new(backend)?
        .max_bytes(1024)
        .allowed_content_types(["text/plain"])
        .allowed_extensions(["txt"])
        .preferred_chunk_size(2))
}

fn ctx() -> StorageContext {
    StorageContext::system("test")
}

fn anon_cookie() -> String {
    format!("{STORAGE_ANON_COOKIE}=test-anon")
}

struct CountingResolver {
    calls: Arc<AtomicUsize>,
}

impl StorageKeyResolver for CountingResolver {
    fn resolve_key<'a>(
        &'a self,
        _ctx: &'a StorageContext,
        _intent: &'a UploadIntent,
    ) -> StorageKeyFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { storage_key() })
    }
}
