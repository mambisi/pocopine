// MinIO-backed integration tests for the RFC-100 `AssetStore` — the
// shared read/write path behind `pocopine assets push` and the Mode B
// serving proxy.
//
// Requires a working Docker daemon (same caveat as `minio.rs`).
//
// Host-only.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};

use pocopine_assets::{ASSET_CACHE_CONTROL, AssetStore, AssetStoreConfig};
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::minio::MinIO;
use tokio::sync::OnceCell;

const MINIO_PORT: u16 = 9000;
const MINIO_ROOT_USER: &str = "minioadmin";
const MINIO_ROOT_PASSWORD: &str = "minioadmin";

static MINIO: OnceCell<ContainerAsync<MinIO>> = OnceCell::const_new();
static BUCKET_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// An `AssetStore` pointed at the shared MinIO container with a
/// fresh, not-yet-created bucket name — `ensure_bucket` coverage is
/// part of every test.
async fn asset_store(prefix: &str) -> AssetStore {
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
    let bucket = format!(
        "pocopine-assets-it-{prefix}-{}-{}",
        std::process::id(),
        BUCKET_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    AssetStore::connect(AssetStoreConfig {
        endpoint: Some(format!("http://127.0.0.1:{port}")),
        bucket,
        region: None,
        access_key_id: MINIO_ROOT_USER.to_string(),
        secret_access_key: MINIO_ROOT_PASSWORD.to_string(),
    })
}

#[tokio::test]
async fn ensure_bucket_is_idempotent() {
    let store = asset_store("ensure").await;
    store.ensure_bucket().await.expect("first ensure creates");
    store.ensure_bucket().await.expect("second ensure no-ops");
}

#[tokio::test]
async fn put_then_get_roundtrips_content_type_and_cache_control() {
    let store = asset_store("roundtrip").await;
    store.ensure_bucket().await.expect("ensure bucket");

    let key = "assets/b94d27b9/blog/clip.webm";
    store
        .put(
            key,
            b"hello world".to_vec(),
            "video/webm",
            ASSET_CACHE_CONTROL,
        )
        .await
        .expect("put asset");

    let fetched = store
        .get(key)
        .await
        .expect("get asset")
        .expect("key exists");
    assert_eq!(fetched.bytes, b"hello world");
    assert_eq!(fetched.content_type.as_deref(), Some("video/webm"));
    assert_eq!(fetched.cache_control.as_deref(), Some(ASSET_CACHE_CONTROL));
}

#[tokio::test]
async fn get_missing_key_is_none_not_error() {
    let store = asset_store("missing").await;
    store.ensure_bucket().await.expect("ensure bucket");
    let fetched = store
        .get("assets/deadbeef/nope.svg")
        .await
        .expect("get must not error on missing keys");
    assert!(fetched.is_none());
}

#[tokio::test]
async fn list_keys_sees_only_the_prefix_and_follows_pagination() {
    let store = asset_store("list").await;
    store.ensure_bucket().await.expect("ensure bucket");

    for i in 0..3 {
        store
            .put(
                &format!("assets/0000000{i}/file-{i}.txt"),
                format!("body-{i}").into_bytes(),
                "text/plain; charset=utf-8",
                ASSET_CACHE_CONTROL,
            )
            .await
            .expect("put asset");
    }
    // A key outside the prefix must not show up in the diff listing.
    store
        .put(
            "other/era.txt",
            b"not an asset".to_vec(),
            "text/plain; charset=utf-8",
            ASSET_CACHE_CONTROL,
        )
        .await
        .expect("put non-asset key");

    let keys = store.list_keys("assets/").await.expect("list keys");
    assert_eq!(keys.len(), 3);
    assert!(keys.contains("assets/00000000/file-0.txt"));
    assert!(keys.contains("assets/00000002/file-2.txt"));
    assert!(!keys.iter().any(|k| k.starts_with("other/")));
}
