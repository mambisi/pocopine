//! RFC-100 Mode B — the private-bucket asset proxy.
//!
//! When the asset bucket is **private** (Railway Buckets are
//! private-only), the browser can't fetch `assets/<hash8>/<path>`
//! keys directly. Instead, asset URLs stay on the app's own origin
//! (the default `/assets` base) and the web service proxies them:
//!
//! ```text
//! browser ── GET /assets/a3f81c2d/blog/clip.webm ──▶ web service
//!                                                       │ S3 GET
//!                                                       ▼
//!                                       bucket key assets/a3f81c2d/blog/clip.webm
//! ```
//!
//! The route installs **automatically** in [`crate::Server::new`]
//! when `POCOPINE_ASSETS_BUCKET` is set — zero code in the app's
//! `main`, which is what lets the Railway adapter light it up by
//! provisioning a bucket and setting env vars. Env contract
//! (deploy-time names shared with `pocopine assets push`'s CI
//! override):
//!
//! | env var | meaning |
//! |---|---|
//! | `POCOPINE_ASSETS_BUCKET` | bucket name; presence enables the proxy |
//! | `POCOPINE_ASSETS_ENDPOINT` | S3-compatible endpoint URL (omit for AWS S3) |
//! | `POCOPINE_ASSETS_REGION` | signing region (default `us-east-1`) |
//! | `POCOPINE_ASSETS_ACCESS_KEY_ID` | static access key id (required) |
//! | `POCOPINE_ASSETS_SECRET_ACCESS_KEY` | static secret access key (required) |
//!
//! # No server-side hash verification (deliberate)
//!
//! The dev server re-hashes file bytes and answers `409` on mismatch
//! because it serves from the **mutable working tree**, where the
//! file can drift from the hash compiled into the binary. Here the
//! bucket key *is* the hash: `pocopine assets push` computed it from
//! the exact bytes it uploaded, keys are immutable by convention, and
//! the response carries the immutable cache header. Re-hashing every
//! response would buffer + burn CPU to compare the store against
//! itself; a URL whose hash has no matching key simply 404s.
//!
//! # v1 limits (documented follow-ups, RFC-100 §10)
//!
//! * Bodies are buffered in memory (`AssetStore::get` collects);
//!   fine for site media, not for multi-GB objects. Streaming reads
//!   need a public streaming surface on `pocopine-storage-s3`.
//! * `Range` requests are answered with the full body (`200`), which
//!   HTTP permits; proper `206` support rides the same streaming
//!   follow-up.

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use pocopine_assets::{AssetStore, AssetStoreConfig, FetchedAsset, ASSET_CACHE_CONTROL};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Bucket name env var; presence (non-empty) enables the proxy.
pub const ASSETS_BUCKET_ENV: &str = "POCOPINE_ASSETS_BUCKET";
/// Optional S3-compatible endpoint URL env var.
pub const ASSETS_ENDPOINT_ENV: &str = "POCOPINE_ASSETS_ENDPOINT";
/// Optional signing-region env var.
pub const ASSETS_REGION_ENV: &str = "POCOPINE_ASSETS_REGION";
/// Access key id env var (required once the bucket is set).
pub const ASSETS_ACCESS_KEY_ID_ENV: &str = "POCOPINE_ASSETS_ACCESS_KEY_ID";
/// Secret access key env var (required once the bucket is set).
pub const ASSETS_SECRET_ACCESS_KEY_ENV: &str = "POCOPINE_ASSETS_SECRET_ACCESS_KEY";

/// Install the Mode B proxy on `router` when the env contract says
/// so. Called from [`crate::Server::new`]; not part of the public
/// surface (apps configure via env, not code — one pattern).
pub(crate) fn install_assets_proxy(router: Router) -> Router {
    match source_from_env() {
        EnvOutcome::Disabled => router,
        EnvOutcome::Enabled(store) => {
            tracing::info!(
                target: "pocopine.log",
                bucket = %store.bucket(),
                "asset proxy enabled: GET /assets/<hash>/<path> serves the private bucket"
            );
            router.merge(proxy_router(Arc::new(store)))
        }
        EnvOutcome::Misconfigured(message) => {
            // Fail loud but keep the server bootable: every asset URL
            // answers 503 naming the missing variable, instead of the
            // app silently 404ing all media.
            tracing::error!(target: "pocopine.log", error = %message, "asset proxy misconfigured");
            router.route(
                "/assets/:hash/*path",
                get(move || {
                    let message = message.clone();
                    async move { (StatusCode::SERVICE_UNAVAILABLE, message) }
                }),
            )
        }
    }
}

enum EnvOutcome {
    /// `POCOPINE_ASSETS_BUCKET` unset/empty — the app serves no
    /// proxied assets (Mode A deploys and bucket-less apps land here).
    Disabled,
    Enabled(AssetStore),
    /// Bucket set but credentials incomplete.
    Misconfigured(String),
}

fn source_from_env() -> EnvOutcome {
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let Some(bucket) = env(ASSETS_BUCKET_ENV) else {
        return EnvOutcome::Disabled;
    };
    let (Some(access_key_id), Some(secret_access_key)) = (
        env(ASSETS_ACCESS_KEY_ID_ENV),
        env(ASSETS_SECRET_ACCESS_KEY_ENV),
    ) else {
        return EnvOutcome::Misconfigured(format!(
            "asset proxy: ${ASSETS_BUCKET_ENV} is set but ${ASSETS_ACCESS_KEY_ID_ENV} / \
             ${ASSETS_SECRET_ACCESS_KEY_ENV} are not — set both access keys"
        ));
    };
    EnvOutcome::Enabled(AssetStore::connect(AssetStoreConfig {
        endpoint: env(ASSETS_ENDPOINT_ENV),
        bucket,
        region: env(ASSETS_REGION_ENV),
        access_key_id,
        secret_access_key,
    }))
}

/// Internal seam so the route handler is testable without a bucket:
/// production uses [`AssetStore`]; tests stub an in-memory map.
trait ObjectSource: Send + Sync + 'static {
    fn fetch(
        &self,
        key: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<FetchedAsset>, String>> + Send>>;
}

impl ObjectSource for AssetStore {
    fn fetch(
        &self,
        key: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<FetchedAsset>, String>> + Send>> {
        let store = self.clone();
        Box::pin(async move { store.get(&key).await.map_err(|err| err.to_string()) })
    }
}

/// `GET /assets/:hash/*path` over any [`ObjectSource`].
fn proxy_router(source: Arc<dyn ObjectSource>) -> Router {
    Router::new().route("/assets/:hash/*path", get(serve_asset).with_state(source))
}

async fn serve_asset(
    State(source): State<Arc<dyn ObjectSource>>,
    AxumPath((hash, path)): AxumPath<(String, String)>,
) -> Response {
    // Same hash-segment shape as the dev server and the `asset!`
    // macro: exactly 8 lowercase hex chars. Anything else is not an
    // asset URL this route owns.
    if !is_asset_hash(&hash) || path.is_empty() {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let key = format!("assets/{hash}/{path}");
    match source.fetch(key.clone()).await {
        Ok(Some(asset)) => {
            let content_type = asset
                .content_type
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let cache_control = asset
                .cache_control
                .unwrap_or_else(|| ASSET_CACHE_CONTROL.to_string());
            (
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::CACHE_CONTROL, cache_control),
                ],
                asset.bytes,
            )
                .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(error) => {
            tracing::error!(
                target: "pocopine.log",
                key = %key,
                %error,
                "asset proxy: bucket fetch failed"
            );
            (StatusCode::BAD_GATEWAY, "asset store unavailable").into_response()
        }
    }
}

/// True for an 8-char lowercase-hex hash segment — the shape
/// `pocopine_core::assets::asset_hash` produces.
fn is_asset_hash(segment: &str) -> bool {
    segment.len() == 8
        && segment
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::collections::HashMap;
    use tower::ServiceExt;

    struct StubSource {
        objects: HashMap<String, FetchedAsset>,
        fail: bool,
    }

    impl ObjectSource for StubSource {
        fn fetch(
            &self,
            key: String,
        ) -> Pin<Box<dyn Future<Output = Result<Option<FetchedAsset>, String>> + Send>> {
            let result = if self.fail {
                Err("boom".to_string())
            } else {
                Ok(self.objects.get(&key).cloned())
            };
            Box::pin(async move { result })
        }
    }

    fn stub_router(fail: bool) -> Router {
        let mut objects = HashMap::new();
        objects.insert(
            "assets/b94d27b9/blog/clip.webm".to_string(),
            FetchedAsset {
                bytes: b"hello world".to_vec(),
                content_type: Some("video/webm".to_string()),
                cache_control: Some(ASSET_CACHE_CONTROL.to_string()),
            },
        );
        objects.insert(
            "assets/deadbeef/bare.bin".to_string(),
            FetchedAsset {
                bytes: b"bare".to_vec(),
                content_type: None,
                cache_control: None,
            },
        );
        proxy_router(Arc::new(StubSource { objects, fail }))
    }

    async fn get_status_and_headers(
        router: Router,
        uri: &str,
    ) -> (StatusCode, axum::http::HeaderMap) {
        let response = router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .expect("oneshot");
        (response.status(), response.headers().clone())
    }

    #[tokio::test]
    async fn serves_object_with_stored_content_type_and_cache_control() {
        let (status, headers) =
            get_status_and_headers(stub_router(false), "/assets/b94d27b9/blog/clip.webm").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE.as_str()], "video/webm");
        assert_eq!(headers[header::CACHE_CONTROL.as_str()], ASSET_CACHE_CONTROL);
    }

    #[tokio::test]
    async fn falls_back_to_octet_stream_and_immutable_when_metadata_missing() {
        let (status, headers) =
            get_status_and_headers(stub_router(false), "/assets/deadbeef/bare.bin").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers[header::CONTENT_TYPE.as_str()],
            "application/octet-stream"
        );
        assert_eq!(headers[header::CACHE_CONTROL.as_str()], ASSET_CACHE_CONTROL);
    }

    #[tokio::test]
    async fn missing_key_is_404() {
        let (status, _) =
            get_status_and_headers(stub_router(false), "/assets/b94d27b9/missing.svg").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn non_hash_segment_is_404_without_touching_the_store() {
        // `fail: true` would 502 on any fetch — proving the handler
        // rejected the URL shape before reaching the bucket.
        let (status, _) =
            get_status_and_headers(stub_router(true), "/assets/DEADBEEF/logo.svg").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get_status_and_headers(stub_router(true), "/assets/abc/logo.svg").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn backend_failure_is_502() {
        let (status, _) =
            get_status_and_headers(stub_router(true), "/assets/b94d27b9/blog/clip.webm").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn hash_segment_shape() {
        assert!(is_asset_hash("b94d27b9"));
        assert!(!is_asset_hash("B94D27B9"));
        assert!(!is_asset_hash("b94d27b"));
        assert!(!is_asset_hash("b94d27b9a"));
        assert!(!is_asset_hash("b94d27bg"));
    }
}
