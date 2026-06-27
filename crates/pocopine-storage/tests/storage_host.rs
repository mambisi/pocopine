#![cfg(not(target_arch = "wasm32"))]

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use pocopine_core::ServerError;
use pocopine_server::RequestContext;
use pocopine_server::auth::{AuthUser, RequestAuthExt};
use pocopine_server::axum::Router;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use pocopine_storage::{
    ChecksumAlgorithm, ChecksumPolicy, CompleteUpload, InitiateUpload, InitiateUploadRequest,
    LocalFsStorageBackend, MemoryStorageBackend, ObjectBody, ObjectChecksum, ObjectMetadata,
    ObjectOwnerRef, ObjectVisibility, ReadUrlRequest, STORAGE_ANON_COOKIE, STORAGE_UPLOADS_PATH,
    SafeObjectKey, SignedRead, StorageActor, StorageBackend, StorageContext, StorageError,
    StorageGuardFuture, StorageKey, StorageKeyFuture, StorageKeyResolver, StorageObjectAccess,
    StorageObjectGuard, StorageResponse, StorageResult, StorageScope, StorageServer, UploadIntent,
    UploadPolicy, UploadSession, UploadSessionId, UploadSessionStatus, UploadStrategy,
    storage_server_plugin, storage_tus_server_plugin,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
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
async fn route_mints_anonymous_binding_cookie_for_public_uploads() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(STORAGE_UPLOADS_PATH)
                .header("content-type", "application/json")
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(
                    serde_json::to_string(&initiate_request("avatars", UploadStrategy::Auto))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .expect("storage response should bind anonymous uploads");
    assert!(set_cookie.starts_with(STORAGE_ANON_COOKIE));
    assert!(set_cookie.contains("Path=/__pocopine/storage"));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Secure"));

    let outer: StorageResponse<UploadSession> =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let session = outer.into_result()?;
    assert_eq!(session.status, UploadSessionStatus::Open);
    Ok(())
}

#[tokio::test]
async fn object_body_stream_limit_fails_when_stream_exceeds_policy() {
    let chunks = futures_util::stream::iter([
        Ok(Bytes::from_static(b"abc")),
        Ok(Bytes::from_static(b"def")),
    ]);
    let body = ObjectBody::from_stream_with_limit(chunks, 5);

    let result = Body::from_stream(body).collect().await;

    assert!(result.is_err());
}

#[tokio::test]
async fn route_can_disable_secure_anonymous_cookie_for_local_http_dev() -> StorageResult<()> {
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .public_scope("avatars", policy("memory")?)?
        .secure_anonymous_cookies(false)
        .build();
    let router = finalize(storage);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(STORAGE_UPLOADS_PATH)
                .header("content-type", "application/json")
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(
                    serde_json::to_string(&initiate_request("avatars", UploadStrategy::Auto))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .expect("storage response should bind anonymous uploads");
    assert!(!set_cookie.contains("Secure"));
    Ok(())
}

#[tokio::test]
async fn route_rejects_mutations_without_csrf_proof() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let (status, rejected): (StatusCode, StorageResult<UploadSession>) = post_json_with_headers(
        router,
        STORAGE_UPLOADS_PATH,
        &initiate_request("avatars", UploadStrategy::Auto),
        true,
        std::iter::empty::<(&str, &str)>(),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(matches!(rejected, Err(StorageError::Forbidden { .. })));
    Ok(())
}

#[tokio::test]
async fn route_errors_use_explicit_response_envelope() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(STORAGE_UPLOADS_PATH)
                .header("content-type", "application/json")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(
                    serde_json::to_string(&initiate_request("unknown", UploadStrategy::Auto))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains(r#""ok":false"#));
    assert!(body.contains(r#""error":"#));
    assert!(!body.contains(r#""Err":"#));
    Ok(())
}

#[tokio::test]
async fn route_rejects_upload_json_without_content_type_before_body_parse() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(STORAGE_UPLOADS_PATH)
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(
                    serde_json::to_string(&initiate_request("avatars", UploadStrategy::Auto))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: StorageResponse<UploadSession> = serde_json::from_slice(&bytes).unwrap();
    assert!(matches!(
        outer.into_result(),
        Err(StorageError::InvalidValue { field, .. }) if field == "Content-Type"
    ));
    Ok(())
}

#[tokio::test]
async fn route_rejects_cross_origin_mutations() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let (status, rejected): (StatusCode, StorageResult<UploadSession>) = post_json_with_headers(
        router,
        STORAGE_UPLOADS_PATH,
        &initiate_request("avatars", UploadStrategy::Auto),
        true,
        [("sec-fetch-site", "cross-site")],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(matches!(rejected, Err(StorageError::Forbidden { .. })));
    Ok(())
}

#[tokio::test]
async fn route_rejects_scheme_mismatched_origins() -> StorageResult<()> {
    let router = finalize(memory_storage()?);
    let (status, rejected): (StatusCode, StorageResult<UploadSession>) = post_json_with_headers(
        router,
        STORAGE_UPLOADS_PATH,
        &initiate_request("avatars", UploadStrategy::Auto),
        true,
        [("host", "app.example"), ("origin", "http://app.example")],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(matches!(rejected, Err(StorageError::Forbidden { .. })));
    Ok(())
}

#[tokio::test]
async fn route_accepts_configured_trusted_origin() -> StorageResult<()> {
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .trusted_origin("http://app.example")?
        .public_scope("avatars", policy("memory")?)?
        .build();
    let router = finalize(storage);
    let (status, session): (StatusCode, StorageResult<UploadSession>) = post_json_with_headers(
        router,
        STORAGE_UPLOADS_PATH,
        &initiate_request("avatars", UploadStrategy::Auto),
        true,
        [("host", "app.example"), ("origin", "http://app.example")],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(session?.status, UploadSessionStatus::Open);
    Ok(())
}

#[tokio::test]
async fn object_read_url_serves_completed_object_and_delete_removes_it() -> StorageResult<()> {
    let storage = memory_storage()?;
    let actor = anon_ctx();
    let session = storage
        .initiate_upload(
            actor.clone(),
            initiate_request("avatars", UploadStrategy::Auto),
        )
        .await?;
    storage
        .append_upload_bytes(
            actor.clone(),
            session.id.clone(),
            0,
            Bytes::from_static(b"hello"),
        )
        .await?;
    let object = storage
        .complete_upload(
            actor,
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    let router = finalize(storage);
    let read_url = format!(
        "/__pocopine/storage/v1/scopes/avatars/objects/read-url/{}",
        object.key
    );
    let signed: SignedRead =
        post_json(router.clone(), &read_url, &ReadUrlRequest::default()).await?;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&signed.url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/plain"
    );
    assert_eq!(response.headers().get("content-length").unwrap(), "5");
    assert_eq!(
        response.headers().get("content-disposition").unwrap(),
        "inline"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"hello");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(&signed.url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-length").unwrap(), "5");
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/__pocopine/storage/v1/scopes/avatars/objects/{}",
                    object.key
                ))
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains(r#""ok":true"#));

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&signed.url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn object_read_url_variant_query_reads_suffix_object() -> StorageResult<()> {
    let scope = StorageScope::builder(policy("memory")?.visibility(ObjectVisibility::Public))
        .key_resolver(LinkedImageResolver)
        .build();
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .scope("avatars", scope)?
        .build();
    let actor = anon_ctx();

    let original_session = storage
        .initiate_upload(
            actor.clone(),
            initiate_request("avatars", UploadStrategy::Auto),
        )
        .await?;
    storage
        .append_upload_bytes(
            actor.clone(),
            original_session.id.clone(),
            0,
            Bytes::from_static(b"hello"),
        )
        .await?;
    let original = storage
        .complete_upload(
            actor.clone(),
            CompleteUpload {
                session: original_session.id,
                checksum: None,
            },
        )
        .await?;

    let mut variant_request = initiate_request("avatars", UploadStrategy::Auto);
    variant_request.file_name = "photo.thumb64.txt".to_string();
    variant_request.metadata = [
        (
            pocopine_storage::IMAGE_VARIANT_BASE_KEY_METADATA_KEY.to_string(),
            original.key.clone(),
        ),
        (
            pocopine_storage::IMAGE_VARIANT_METADATA_KEY.to_string(),
            "thumb64".to_string(),
        ),
    ]
    .into();
    let variant_session = storage
        .initiate_upload(actor.clone(), variant_request)
        .await?;
    storage
        .append_upload_bytes(
            actor,
            variant_session.id.clone(),
            0,
            Bytes::from_static(b"small"),
        )
        .await?;
    let variant = storage
        .complete_upload(
            anon_ctx(),
            CompleteUpload {
                session: variant_session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(variant.key, "avatars/user-1/photo.thumb64.jpg");

    let router = finalize(storage);
    let read_url = format!(
        "/__pocopine/storage/v1/scopes/avatars/objects/read-url/{}",
        original.key
    );
    let signed: SignedRead = post_json(
        router.clone(),
        &read_url,
        &ReadUrlRequest::default().variant("thumb64"),
    )
    .await?;
    assert!(signed.url.contains("variant=thumb64"));

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&signed.url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"small")
    );
    Ok(())
}

#[tokio::test]
async fn object_delete_variant_query_removes_suffix_object_only() -> StorageResult<()> {
    let scope = StorageScope::builder(policy("memory")?.visibility(ObjectVisibility::Public))
        .key_resolver(LinkedImageResolver)
        .build();
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .scope("avatars", scope)?
        .build();
    let actor = anon_ctx();

    let original_session = storage
        .initiate_upload(
            actor.clone(),
            initiate_request("avatars", UploadStrategy::Auto),
        )
        .await?;
    storage
        .append_upload_bytes(
            actor.clone(),
            original_session.id.clone(),
            0,
            Bytes::from_static(b"hello"),
        )
        .await?;
    let original = storage
        .complete_upload(
            actor.clone(),
            CompleteUpload {
                session: original_session.id,
                checksum: None,
            },
        )
        .await?;

    let mut variant_request = initiate_request("avatars", UploadStrategy::Auto);
    variant_request.file_name = "photo.thumb64.txt".to_string();
    variant_request.metadata = [
        (
            pocopine_storage::IMAGE_VARIANT_BASE_KEY_METADATA_KEY.to_string(),
            original.key.clone(),
        ),
        (
            pocopine_storage::IMAGE_VARIANT_METADATA_KEY.to_string(),
            "thumb64".to_string(),
        ),
    ]
    .into();
    let variant_session = storage
        .initiate_upload(actor.clone(), variant_request)
        .await?;
    storage
        .append_upload_bytes(
            actor,
            variant_session.id.clone(),
            0,
            Bytes::from_static(b"small"),
        )
        .await?;
    storage
        .complete_upload(
            anon_ctx(),
            CompleteUpload {
                session: variant_session.id,
                checksum: None,
            },
        )
        .await?;

    let router = finalize(storage.clone());
    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/__pocopine/storage/v1/scopes/avatars/objects/{}?variant=thumb64",
                    original.key
                ))
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (read, _) = storage
        .read_object(
            StorageContext::system("test"),
            "avatars".to_string(),
            SafeObjectKey::parse(&original.key)?,
            None,
            None,
        )
        .await?;
    assert_eq!(
        Body::from_stream(read.body)
            .collect()
            .await
            .unwrap()
            .to_bytes(),
        Bytes::from_static(b"hello")
    );
    let deleted_variant = storage
        .read_object(
            StorageContext::system("test"),
            "avatars".to_string(),
            SafeObjectKey::parse(&original.key)?,
            None,
            Some("thumb64".to_string()),
        )
        .await;
    assert!(matches!(
        deleted_variant,
        Err(StorageError::UnknownObject { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn private_scope_rejects_browser_object_access_without_key_guard() -> StorageResult<()> {
    let scope = StorageScope::builder(policy("memory")?)
        .key_resolver(CountingResolver {
            calls: Arc::new(AtomicUsize::new(0)),
        })
        .build();
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .scope("avatars", scope)?
        .build();
    let actor = ctx();
    let session = storage
        .initiate_upload(
            actor.clone(),
            initiate_request("avatars", UploadStrategy::Auto),
        )
        .await?;
    storage
        .append_upload_bytes(
            actor.clone(),
            session.id.clone(),
            0,
            Bytes::from_static(b"hello"),
        )
        .await?;
    let object = storage
        .complete_upload(
            actor,
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;

    let router = finalize(storage.clone());
    let object_uri = format!(
        "/__pocopine/storage/v1/scopes/avatars/objects/{}",
        object.key
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&object_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let read_url = format!(
        "/__pocopine/storage/v1/scopes/avatars/objects/read-url/{}",
        object.key
    );
    let (status, signed): (StatusCode, StorageResult<SignedRead>) =
        post_json_status(router.clone(), &read_url, &ReadUrlRequest::default(), true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(matches!(signed, Err(StorageError::Forbidden { .. })));

    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&object_uri)
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let (read, _) = storage
        .read_object(
            StorageContext::system("checked-storage-read"),
            "avatars".to_string(),
            SafeObjectKey::parse(&object.key)?,
            None,
            None,
        )
        .await?;
    assert_eq!(
        Body::from_stream(read.body)
            .collect()
            .await
            .unwrap()
            .to_bytes(),
        Bytes::from_static(b"hello")
    );
    Ok(())
}

#[tokio::test]
async fn object_guard_authorizes_read_url_direct_read_and_delete_by_key() -> StorageResult<()> {
    let scope = StorageScope::builder(policy("memory")?)
        .key_resolver(PrincipalKeyResolver)
        .object_guard(OwnerPrefixGuard)
        .build();
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .scope("attachments", scope)?
        .build();
    let owner = principal_ctx("user-1", "owner");
    let other = principal_ctx("user-2", "other");
    let session = storage
        .initiate_upload(
            owner.clone(),
            initiate_request("attachments", UploadStrategy::Auto),
        )
        .await?;
    storage
        .append_upload_bytes(
            owner.clone(),
            session.id.clone(),
            0,
            Bytes::from_static(b"hello"),
        )
        .await?;
    let object = storage
        .complete_upload(
            owner.clone(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    assert_eq!(object.key, "user-1/photo.txt");
    let key = SafeObjectKey::parse(&object.key)?;

    let denied_read = storage
        .read_object(
            other.clone(),
            "attachments".to_string(),
            key.clone(),
            None,
            None,
        )
        .await;
    assert!(matches!(denied_read, Err(StorageError::Forbidden { .. })));

    let denied_signed = storage
        .signed_read(
            other.clone(),
            "attachments".to_string(),
            key.clone(),
            ReadUrlRequest::default(),
        )
        .await;
    assert!(matches!(denied_signed, Err(StorageError::Forbidden { .. })));

    let denied_delete = storage
        .delete_object(other, "attachments".to_string(), key.clone(), None)
        .await;
    assert!(matches!(denied_delete, Err(StorageError::Forbidden { .. })));

    let signed = storage
        .signed_read(
            owner.clone(),
            "attachments".to_string(),
            key.clone(),
            ReadUrlRequest::default(),
        )
        .await?;
    let router = finalize(storage.clone());
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&signed.url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"hello")
    );

    storage
        .delete_object(owner, "attachments".to_string(), key, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn local_fs_read_streams_multichunk_object_with_matching_content_length() -> StorageResult<()>
{
    let tmp = tempdir().unwrap();
    let size = 70 * 1024 + 17;
    let bytes: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
    let policy = UploadPolicy::new("local")?
        .max_bytes(bytes.len() as u64 + 1)
        .allowed_content_types(["text/plain"])
        .allowed_extensions(["txt"])
        .preferred_chunk_size(32 * 1024);
    let storage = StorageServer::builder()
        .backend("local", LocalFsStorageBackend::new(tmp.path()))?
        .public_scope("avatars", policy)?
        .build();
    let actor = anon_ctx();
    let session = storage
        .initiate_upload(
            actor.clone(),
            InitiateUploadRequest {
                protocol: pocopine_storage::STORAGE_PROTOCOL_V1.to_string(),
                scope: "avatars".to_string(),
                file_name: "large.txt".to_string(),
                size: Some(bytes.len() as u64),
                content_type: Some("text/plain".to_string()),
                metadata: Default::default(),
                requested_strategy: UploadStrategy::Sequential,
            },
        )
        .await?;
    storage
        .append_upload_bytes(
            actor.clone(),
            session.id.clone(),
            0,
            Bytes::from(bytes.clone()),
        )
        .await?;
    let object = storage
        .complete_upload(
            actor,
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;
    let router = finalize(storage);
    let read_url = format!(
        "/__pocopine/storage/v1/scopes/avatars/objects/read-url/{}",
        object.key
    );
    let signed: SignedRead =
        post_json(router.clone(), &read_url, &ReadUrlRequest::default()).await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&signed.url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-length").unwrap(),
        &HeaderValue::from_str(&bytes.len().to_string()).unwrap()
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.len(), bytes.len());
    assert_eq!(&body[..], bytes.as_slice());
    Ok(())
}

#[tokio::test]
async fn tus_options_advertises_creation_resume_and_termination() -> StorageResult<()> {
    let router = finalize_tus(memory_storage()?);
    let response = router
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri(tus_uploads_uri("avatars"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers().get("tus-resumable").unwrap(), "1.0.0");
    assert_eq!(response.headers().get("tus-version").unwrap(), "1.0.0");
    assert!(
        response
            .headers()
            .get("tus-extension")
            .and_then(|value| value.to_str().ok())
            .unwrap()
            .contains("creation-defer-length")
    );
    assert_eq!(response.headers().get("tus-max-size").unwrap(), "1024");
    Ok(())
}

#[tokio::test]
async fn tus_create_head_patch_and_delete_use_tus_wire_contract() -> StorageResult<()> {
    let storage = memory_storage()?;
    let router = finalize_tus(storage.clone());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "5")
                .header(
                    "upload-metadata",
                    "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers().get("tus-resumable").unwrap(), "1.0.0");
    assert_eq!(response.headers().get("upload-offset").unwrap(), "0");
    assert_eq!(response.headers().get("upload-length").unwrap(), "5");
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("cookie", anon_cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers().get("upload-offset").unwrap(), "0");
    assert_eq!(response.headers().get("upload-length").unwrap(), "5");
    assert_eq!(
        response.headers().get("upload-metadata").unwrap(),
        "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg=="
    );
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    assert_eq!(response.headers().get("vary").unwrap(), "Cookie");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("upload-offset", "0")
                .header("content-type", "application/offset+octet-stream")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers().get("upload-offset").unwrap(), "5");
    assert!(response.headers().contains_key("upload-expires"));

    let session = UploadSessionId::new(location.rsplit('/').next().unwrap())?;
    let inspected = storage.inspect_upload(anon_ctx(), session.clone()).await?;
    assert_eq!(inspected.status, UploadSessionStatus::Complete);

    let locked = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("upload-offset", "5")
                .header("content-type", "application/offset+octet-stream")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(locked.status(), StatusCode::from_u16(423).unwrap());

    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(matches!(
        storage.inspect_upload(anon_ctx(), session).await,
        Err(StorageError::UnknownUploadSession { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn tus_zero_byte_creation_with_upload_completes() -> StorageResult<()> {
    let storage = memory_storage()?;
    let router = finalize_tus(storage.clone());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "0")
                .header("content-type", "application/offset+octet-stream")
                .header(
                    "upload-metadata",
                    "filename ZW1wdHkudHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers().get("upload-offset").unwrap(), "0");
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap();
    let session = UploadSessionId::new(location.rsplit('/').next().unwrap())?;
    let inspected = storage.inspect_upload(anon_ctx(), session).await?;
    assert_eq!(inspected.status, UploadSessionStatus::Complete);

    let rejected = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "0")
                .header("content-type", "application/offset+octet-stream")
                .header(
                    "upload-metadata",
                    "filename ZW1wdHkudHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("x"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn tus_error_paths_use_tus_status_codes() -> StorageResult<()> {
    let router = finalize_tus(memory_storage()?);
    let create = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("upload-length", "5")
                .header(
                    "upload-metadata",
                    "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(create.headers().get("tus-version").unwrap(), "1.0.0");

    let create = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "5")
                .header(
                    "upload-metadata",
                    "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let location = create
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();

    let wrong_type = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("upload-offset", "0")
                .header("content-type", "application/json")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_type.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let mismatch = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("upload-offset", "1")
                .header("content-type", "application/offset+octet-stream")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::CONFLICT);

    let mut small_policy = policy("memory")?;
    small_policy.max_part_size = Some(4);
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .public_scope("avatars", small_policy)?
        .build();
    let router = finalize_tus(storage);
    let create = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "5")
                .header(
                    "upload-metadata",
                    "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let location = create
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    let too_large = router
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("upload-offset", "0")
                .header("content-type", "application/offset+octet-stream")
                .header("content-length", "5")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn tus_accepts_deferred_upload_length() -> StorageResult<()> {
    let storage = memory_storage()?;
    let router = finalize_tus(storage.clone());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-defer-length", "1")
                .header(
                    "upload-metadata",
                    "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers().get("upload-defer-length").unwrap(), "1");
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("upload-offset", "0")
                .header("upload-length", "5")
                .header("content-type", "application/offset+octet-stream")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers().get("upload-offset").unwrap(), "5");
    assert!(response.headers().contains_key("upload-expires"));

    let response = router
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("cookie", anon_cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers().get("upload-length").unwrap(), "5");
    assert!(response.headers().get("upload-defer-length").is_none());

    let session = UploadSessionId::new(location.rsplit('/').next().unwrap())?;
    let inspected = storage.inspect_upload(anon_ctx(), session).await?;
    assert_eq!(inspected.status, UploadSessionStatus::Complete);
    Ok(())
}

#[tokio::test]
async fn tus_accepts_key_only_metadata_and_rejects_duplicate_keys() -> StorageResult<()> {
    let router = finalize_tus(memory_storage()?);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "5")
                .header(
                    "upload-metadata",
                    "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==,is_confidential",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(&location)
                .header("tus-resumable", "1.0.0")
                .header("cookie", anon_cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers().get("upload-metadata").unwrap(),
        "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==,is_confidential"
    );

    let duplicate = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "5")
                .header("upload-metadata", "filename cGhvdG8=,filename b3RoZXI=")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn tus_post_method_override_can_patch_upload_bytes() -> StorageResult<()> {
    let storage = memory_storage()?;
    let router = finalize_tus(storage.clone());
    let create = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(tus_uploads_uri("avatars"))
                .header("tus-resumable", "1.0.0")
                .header("upload-length", "5")
                .header(
                    "upload-metadata",
                    "filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==",
                )
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let location = create
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&location)
                .header("x-http-method-override", "PATCH")
                .header("tus-resumable", "1.0.0")
                .header("upload-offset", "0")
                .header("content-type", "application/offset+octet-stream")
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers().get("upload-offset").unwrap(), "5");

    let session = UploadSessionId::new(location.rsplit('/').next().unwrap())?;
    let inspected = storage.inspect_upload(anon_ctx(), session).await?;
    assert_eq!(inspected.status, UploadSessionStatus::Complete);
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
async fn predicate_guards_do_not_implicitly_allow_system_context() -> StorageResult<()> {
    let storage = StorageServer::builder()
        .backend("memory", MemoryStorageBackend::new())?
        .guarded_scope(
            "avatars",
            policy("memory")?,
            pocopine_server::auth::require_auth(),
        )?
        .build();

    let denied = storage
        .initiate_upload(ctx(), initiate_request("avatars", UploadStrategy::Auto))
        .await;
    assert!(matches!(denied, Err(StorageError::Unauthorized { .. })));
    Ok(())
}

#[tokio::test]
async fn principal_owner_binding_ignores_mutable_claims() -> StorageResult<()> {
    let backend = MemoryStorageBackend::new();
    let session = initiate_direct_with_ctx(
        &backend,
        &principal_ctx("user-1", "initial"),
        Some(5),
        policy(backend.name())?,
    )
    .await?;

    let inspected = backend
        .inspect_upload(&principal_ctx("user-1", "changed"), session.id)
        .await?;
    assert_eq!(inspected.status, UploadSessionStatus::Open);
    Ok(())
}

#[tokio::test]
async fn server_hides_other_actor_sessions_as_not_found() -> StorageResult<()> {
    // Cross-actor probes must be indistinguishable from probes for a session
    // that never existed; returning Forbidden would let an attacker enumerate
    // session IDs belonging to other users by comparing 403 vs 404 responses.
    let storage = memory_storage()?;
    let session = storage
        .initiate_upload(
            principal_ctx("user-1", "initial"),
            initiate_request("avatars", UploadStrategy::Auto),
        )
        .await?;

    let denied = storage
        .inspect_upload(principal_ctx("user-2", "initial"), session.id.clone())
        .await;
    assert!(matches!(
        denied,
        Err(StorageError::UnknownUploadSession { .. })
    ));

    // abort follows the same no-leak contract: a non-owner sees the same
    // Ok(()) response a missing-session caller would see.
    storage
        .abort_upload(principal_ctx("user-2", "initial"), session.id.clone())
        .await?;
    let inspected = storage
        .inspect_upload(principal_ctx("user-1", "initial"), session.id)
        .await?;
    assert_eq!(inspected.status, UploadSessionStatus::Open);
    Ok(())
}

#[tokio::test]
async fn expired_sessions_close_and_sweep() -> StorageResult<()> {
    let backend = MemoryStorageBackend::new();
    let mut expiring = policy(backend.name())?;
    expiring.expires_after = Duration::from_secs(0);
    let session = initiate_direct_with(&backend, Some(5), expiring).await?;

    let inspected = backend.inspect_upload(&ctx(), session.id.clone()).await?;
    assert_eq!(inspected.status, UploadSessionStatus::Expired);

    let append = backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"h"))
        .await;
    assert!(matches!(append, Err(StorageError::UploadClosed { .. })));
    assert_eq!(backend.sweep_expired_uploads()?, 1);
    assert!(matches!(
        backend.inspect_upload(&ctx(), session.id).await,
        Err(StorageError::UnknownUploadSession { .. })
    ));
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
    assert!(matches!(
        rejected,
        Err(StorageError::PayloadTooLarge { .. })
    ));
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
async fn optional_sha256_checksum_is_verified_when_present() -> StorageResult<()> {
    let backend = MemoryStorageBackend::new();
    let mut checksum_policy = policy(backend.name())?;
    checksum_policy.checksum = ChecksumPolicy::Optional(vec![ChecksumAlgorithm::Sha256]);
    let session = initiate_direct_with(&backend, Some(5), checksum_policy).await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;

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

#[test]
fn md5_and_crc32c_checksum_policies_are_now_supported() -> StorageResult<()> {
    // pocopine-crypto backs all three algorithms, so configuring a scope with a
    // required MD5 or CRC32C checksum no longer fails as unsupported.
    for algorithm in [ChecksumAlgorithm::Md5, ChecksumAlgorithm::Crc32c] {
        let mut checksum_policy = policy("memory")?;
        checksum_policy.checksum = ChecksumPolicy::Required(algorithm);
        let builder = StorageServer::builder().backend("memory", MemoryStorageBackend::new())?;
        assert!(builder.public_scope("avatars", checksum_policy).is_ok());
    }
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
        patch_bytes_outer(router.clone(), &session.id, 0, "again".as_bytes()).await;
    assert!(matches!(
        mismatch,
        Err(StorageError::OffsetMismatch {
            expected: 5,
            provided: 0
        })
    ));

    let over_end: StorageResult<UploadSession> =
        patch_bytes_outer(router, &session.id, 6, "!".as_bytes()).await;
    assert!(matches!(
        over_end,
        Err(StorageError::OffsetMismatch {
            expected: 5,
            provided: 6
        })
    ));
    Ok(())
}

#[tokio::test]
async fn zero_byte_upload_can_complete_without_patch() -> StorageResult<()> {
    let backend = MemoryStorageBackend::new();
    let session = initiate_direct_with(&backend, Some(0), policy(backend.name())?).await?;
    let object = backend
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id,
                checksum: None,
            },
        )
        .await?;

    assert_eq!(object.size, 0);
    assert_eq!(object.key, "avatars/user-1/photo.txt");
    Ok(())
}

#[tokio::test]
async fn local_fs_backend_errors_do_not_expose_absolute_paths() -> StorageResult<()> {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("storage-root");
    std::fs::File::create(&root).unwrap();
    let backend = LocalFsStorageBackend::new(&root);
    let err = initiate_direct_with(&backend, Some(5), policy(backend.name())?)
        .await
        .unwrap_err();
    let StorageError::Backend { message } = err else {
        panic!("expected backend error");
    };

    assert!(message.contains("create storage root"));
    assert!(!message.contains(root.to_str().unwrap()));
    assert!(!message.contains(tmp.path().to_str().unwrap()));
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
async fn local_fs_resume_trusts_persisted_offset_not_temp_length() -> StorageResult<()> {
    let tmp = tempdir().unwrap();
    let backend = LocalFsStorageBackend::new(tmp.path());
    let session = initiate_direct(&backend).await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(
            tmp.path()
                .join(".pocopine-storage")
                .join("sessions")
                .join(session.id.as_str())
                .join("bytes.tmp"),
        )
        .unwrap()
        .write_all(b"torn")
        .unwrap();

    let reloaded = LocalFsStorageBackend::new(tmp.path());
    let inspected = reloaded.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.next_offset, Some(5));
    Ok(())
}

#[tokio::test]
async fn local_fs_inspect_expired_session_does_not_write_metadata() -> StorageResult<()> {
    let tmp = tempdir().unwrap();
    let backend = LocalFsStorageBackend::new(tmp.path());
    let mut expiring = policy(backend.name())?;
    expiring.expires_after = Duration::from_secs(0);
    let session = initiate_direct_with(&backend, Some(5), expiring).await?;
    let meta = tmp
        .path()
        .join(".pocopine-storage")
        .join("sessions")
        .join(session.id.as_str())
        .join("session.json");
    let before = std::fs::read(&meta).unwrap();

    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(inspected.status, UploadSessionStatus::Expired);
    let after = std::fs::read(&meta).unwrap();
    assert_eq!(after, before);
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
async fn local_fs_complete_does_not_adopt_pre_existing_object_for_unfinalized_session()
-> StorageResult<()> {
    // The "tmp is missing, final_path exists" recovery branch used to claim
    // the on-disk object as this session's result. That let a fresh session
    // whose key happened to resolve to an already-occupied object path adopt
    // another upload's bytes. Recovery from a half-committed complete now
    // requires the session metadata itself to record the ObjectRef (the
    // early-return at the top of complete_upload) — losing the temp file
    // without a finalized metadata commit is a genuine failure.
    let tmp = tempdir().unwrap();
    let backend = LocalFsStorageBackend::new(tmp.path());
    let session = initiate_direct(&backend).await?;
    backend
        .append_upload_bytes(&ctx(), session.id.clone(), 0, Bytes::from_static(b"hello"))
        .await?;
    let session_tmp = tmp
        .path()
        .join(".pocopine-storage")
        .join("sessions")
        .join(session.id.as_str())
        .join("bytes.tmp");
    let final_path = tmp.path().join("avatars/user-1/photo.txt");
    std::fs::create_dir_all(final_path.parent().unwrap()).unwrap();
    std::fs::rename(session_tmp, &final_path).unwrap();

    let reloaded = LocalFsStorageBackend::new(tmp.path());
    let result = reloaded
        .complete_upload(
            &ctx(),
            CompleteUpload {
                session: session.id.clone(),
                checksum: None,
            },
        )
        .await;
    assert!(
        result.is_err(),
        "missing temp file must not silently adopt an existing on-disk object"
    );
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

#[test]
fn upload_session_id_rejects_dotted_traversal() {
    // "." and ".." would otherwise pass the original allow-list (alphanumeric
    // + '-_.') and become a filesystem path component in LocalFs.
    for value in [".", "..", "...", "..-..", "_-.-_"] {
        assert!(
            UploadSessionId::new(value).is_err(),
            "all-punctuation session id should be rejected: {value:?}"
        );
    }
    // Sanity-check that normal IDs still work.
    assert!(UploadSessionId::new("session-1.foo_bar").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_fs_serializes_concurrent_appends_on_the_same_session() -> StorageResult<()> {
    // Without the per-session lock, two concurrent PATCHes that both observe
    // `next_offset = 0` would both pass the equality check and both O_APPEND
    // their payload, scrambling bytes on disk and corrupting next_offset.
    let tmp = tempdir().unwrap();
    let backend = Arc::new(LocalFsStorageBackend::new(tmp.path()));
    // Deferred length so a winning 16-byte append does not trip the
    // declared-size check baked into the default `initiate_direct` helper.
    let session = initiate_direct_with(backend.as_ref(), None, policy("local_fs")?).await?;

    let mut handles = Vec::new();
    for shard in 0..8u8 {
        let backend = backend.clone();
        let session_id = session.id.clone();
        handles.push(tokio::spawn(async move {
            backend
                .append_upload_bytes(
                    &ctx(),
                    session_id,
                    // Each task offers offset 0; only one can win per round.
                    0,
                    Bytes::from(vec![shard; 16]),
                )
                .await
        }));
    }

    let mut accepted = 0;
    let mut rejected = 0;
    for handle in handles {
        match handle.await.expect("spawn") {
            Ok(_) => accepted += 1,
            Err(StorageError::OffsetMismatch { .. }) => rejected += 1,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    assert_eq!(accepted, 1, "exactly one append at offset 0 should win");
    assert_eq!(
        rejected, 7,
        "all other appends should see an offset mismatch"
    );

    let inspected = backend.inspect_upload(&ctx(), session.id).await?;
    assert_eq!(
        inspected.next_offset,
        Some(16),
        "committed offset must match exactly one accepted chunk"
    );
    Ok(())
}

#[tokio::test]
async fn server_hides_session_existence_when_cookie_does_not_match() -> StorageResult<()> {
    // A second anonymous binding probing the same session id must not be able
    // to tell the difference between "exists but owned by someone else" and
    // "never existed".
    let storage = memory_storage()?;
    let owner = anon_ctx();
    let session = storage
        .initiate_upload(
            owner.clone(),
            initiate_request("avatars", UploadStrategy::Auto),
        )
        .await?;

    let mut other_headers = HeaderMap::new();
    other_headers.insert(
        "cookie",
        HeaderValue::from_static("pocopine_storage_anon=different-anon"),
    );
    let other = StorageContext::from_request(RequestContext::new(
        Method::GET,
        Uri::from_static("/storage-test"),
        other_headers,
    ));

    let probe = storage.inspect_upload(other, session.id).await;
    assert!(
        matches!(probe, Err(StorageError::UnknownUploadSession { .. })),
        "cross-actor probe should be indistinguishable from missing session"
    );
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

fn finalize_tus(storage: StorageServer) -> Router {
    pocopine_server::__reset_for_test();
    pocopine_server::Server::new(Router::new())
        .plugin(storage_tus_server_plugin(storage))
        .try_finalize()
        .unwrap()
}

fn tus_uploads_uri(scope: &str) -> String {
    format!("/__pocopine/storage/tus/v1/{scope}/uploads")
}

async fn post_json<T, R>(router: Router, uri: &str, payload: &T) -> StorageResult<R>
where
    T: Serialize,
    R: DeserializeOwned,
{
    post_json_inner(router, uri, payload, true).await
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
    let (_status, result) = post_json_status(router, uri, payload, include_cookie).await;
    result
}

async fn post_json_status<T, R>(
    router: Router,
    uri: &str,
    payload: &T,
    include_cookie: bool,
) -> (StatusCode, StorageResult<R>)
where
    T: Serialize,
    R: DeserializeOwned,
{
    post_json_with_headers(
        router,
        uri,
        payload,
        include_cookie,
        [("sec-fetch-site", "same-origin")],
    )
    .await
}

async fn post_json_with_headers<T, R>(
    router: Router,
    uri: &str,
    payload: &T,
    include_cookie: bool,
    headers: impl IntoIterator<Item = (&'static str, &'static str)>,
) -> (StatusCode, StorageResult<R>)
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
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let response = router
        .oneshot(
            builder
                .body(Body::from(serde_json::to_string(payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: StorageResponse<R> = serde_json::from_slice(&bytes).unwrap();
    (status, outer.into_result())
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
                    "/__pocopine/storage/v1/uploads/{}",
                    session.as_str()
                ))
                .header("Upload-Offset", offset.to_string())
                .header("cookie", anon_cookie())
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(bytes.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: StorageResponse<R> = serde_json::from_slice(&bytes).unwrap();
    outer.into_result()
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
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: StorageResponse<()> = serde_json::from_slice(&bytes).unwrap();
    outer.into_result()
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
    initiate_direct_with_ctx(backend, &ctx(), size, policy).await
}

async fn initiate_direct_with_ctx<B>(
    backend: &B,
    ctx: &StorageContext,
    size: Option<u64>,
    policy: UploadPolicy,
) -> StorageResult<UploadSession>
where
    B: StorageBackend,
{
    backend
        .initiate_upload(
            ctx,
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
            .metadata(ObjectMetadata::from_entries([("kind", "avatar")])),
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

fn anon_ctx() -> StorageContext {
    let mut headers = HeaderMap::new();
    headers.insert("cookie", HeaderValue::from_str(&anon_cookie()).unwrap());
    StorageContext::from_request(RequestContext::new(
        Method::GET,
        Uri::from_static("/storage-test"),
        headers,
    ))
}

fn principal_ctx(id: &str, claim: &str) -> StorageContext {
    let mut claims = std::collections::BTreeMap::new();
    claims.insert("mutable".to_string(), serde_json::json!(claim));
    let mut user = AuthUser::new(id);
    user.claims = claims;
    StorageContext::from_request(
        RequestContext::new(
            Method::GET,
            Uri::from_static("/storage-test"),
            HeaderMap::new(),
        )
        .with_user(user),
    )
}

fn anon_cookie() -> String {
    format!("{STORAGE_ANON_COOKIE}=test-anon")
}

struct CountingResolver {
    calls: Arc<AtomicUsize>,
}

struct LinkedImageResolver;
struct PrincipalKeyResolver;
struct OwnerPrefixGuard;

impl StorageKeyResolver for LinkedImageResolver {
    fn resolve_key<'a>(
        &'a self,
        _ctx: &'a StorageContext,
        intent: &'a UploadIntent,
    ) -> StorageKeyFuture<'a> {
        Box::pin(async move {
            if let Some(key) = intent.image_variant_object_key()? {
                return Ok(StorageKey::new(key));
            }
            storage_key()
        })
    }
}

impl StorageKeyResolver for PrincipalKeyResolver {
    fn resolve_key<'a>(
        &'a self,
        ctx: &'a StorageContext,
        intent: &'a UploadIntent,
    ) -> StorageKeyFuture<'a> {
        Box::pin(async move {
            let principal = ctx.require_principal()?;
            Ok(StorageKey::new(SafeObjectKey::parse(format!(
                "{}/{}",
                principal.subject,
                intent.file_name()
            ))?))
        })
    }
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

impl StorageObjectGuard for OwnerPrefixGuard {
    fn check(&self, ctx: StorageContext, access: StorageObjectAccess) -> StorageGuardFuture<'_> {
        Box::pin(async move {
            let subject = match ctx.actor {
                StorageActor::Principal(principal) => principal.subject,
                _ => return Err(ServerError::unauthorized("login required")),
            };
            let prefix = format!("{subject}/");
            match access {
                StorageObjectAccess::Write { .. } => Ok(()),
                StorageObjectAccess::Read { key, .. } | StorageObjectAccess::Delete { key, .. } => {
                    if key.as_str().starts_with(&prefix) {
                        Ok(())
                    } else {
                        Err(ServerError::forbidden(
                            "object belongs to another principal",
                        ))
                    }
                }
            }
        })
    }
}
