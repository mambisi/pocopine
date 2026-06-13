//! Cache-aware static file service — [`crate::static_files`].
//!
//! `pocopine build` content-hashes the wasm-pack output pair to
//! `<name>.<hash8>.js` / `<name>_bg.<hash8>.wasm` (one hash — the
//! wasm's — so the glue and the module it instantiates version
//! together; see `build::hash_pkg_bundle` in pocopine-cli). That
//! naming is only safe if the HTTP layer cooperates:
//!
//! * hashed bundle files → `Cache-Control: public,max-age=31536000,
//!   immutable` — a new bundle is a new URL, the old one never goes
//!   stale, and CDNs may cache forever.
//! * HTML (index.html and any SPA fallback that serves it) →
//!   `Cache-Control: no-cache` — revalidate on every load so the
//!   entry point always points at the current pair.
//!
//! Without explicit headers, CDNs apply default heuristics that cache
//! `.js` but not `.wasm` (Cloudflare edge-caches `.js` for hours and
//! passes `.wasm` through), so a deploy could pair a stale cached glue
//! with a fresh wasm — `WebAssembly.instantiate` then fails with
//! `LinkError: … function import requires a callable`.
//!
//! [`StaticFiles`] wraps [`ServeDir`] and applies that policy to every
//! response, fallback responses included. Responses that already carry
//! a `Cache-Control` header are left untouched.
//!
//! It also serves the GENERATED index: `pocopine build` writes the
//! hash-rewritten entry point to `pkg/index.html` (the source
//! `index.html` keeps the stable unhashed `pkg/<name>.js` reference —
//! the hash is build output, not source). Requests for `/` or
//! `/index.html` answer with `pkg/index.html` when it exists and fall
//! back to the source file otherwise, so an unbuilt checkout still
//! serves.

use std::convert::Infallible;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::http::{header, HeaderValue, Request, Response, Uri};
use pocopine_assets::{is_hashed_bundle_name, ASSET_CACHE_CONTROL};
use tower::Service;
use tower_http::services::fs::{DefaultServeDirFallback, ServeFileSystemResponseBody};
use tower_http::services::ServeDir;

/// [`ServeDir`] with the pocopine cache-header policy (module docs).
/// Returned by [`crate::static_files`].
#[derive(Clone, Debug)]
pub struct StaticFiles<F = DefaultServeDirFallback> {
    inner: ServeDir<F>,
    root: PathBuf,
}

impl StaticFiles {
    pub(crate) fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            inner: ServeDir::new(&dir),
            root: dir.as_ref().to_path_buf(),
        }
    }
}

impl<F> StaticFiles<F> {
    /// Set a fallback service — see [`ServeDir::fallback`]. The
    /// fallback's response flows through the same cache-header policy,
    /// so an SPA history fallback that answers with index.html gets
    /// `no-cache` like a direct index.html hit.
    pub fn fallback<F2>(self, new_fallback: F2) -> StaticFiles<F2> {
        StaticFiles {
            inner: self.inner.fallback(new_fallback),
            root: self.root,
        }
    }
}

/// Where `pocopine build` writes the hash-rewritten index.html,
/// relative to the static root (module docs).
const GENERATED_INDEX: &str = "pkg/index.html";

/// True when `path` asks for the entry-point index and the build has
/// generated `pkg/index.html` under `root` — the request is then
/// re-pointed at the generated copy.
fn prefers_generated_index(root: &Path, path: &str) -> bool {
    matches!(path, "/" | "/index.html") && root.join(GENERATED_INDEX).is_file()
}

/// Resolve the index.html a server should hand out as its SPA history
/// fallback: the generated `pkg/index.html` when a build has produced
/// one (it carries the hashed bundle reference), else the source
/// `index.html` (stable unhashed reference — a checkout without a
/// build). Pair it with `tower_http::services::ServeFile`, or call it
/// per request to pick up a build landing under a running server.
pub fn index_file(dir: impl AsRef<Path>) -> PathBuf {
    let root = dir.as_ref();
    let generated = root.join(GENERATED_INDEX);
    if generated.is_file() {
        generated
    } else {
        root.join("index.html")
    }
}

impl<ReqBody, F> Service<Request<ReqBody>> for StaticFiles<F>
where
    ServeDir<F>: Service<
        Request<ReqBody>,
        Response = Response<ServeFileSystemResponseBody>,
        Error = Infallible,
    >,
    <ServeDir<F> as Service<Request<ReqBody>>>::Future: Send + 'static,
{
    type Response = Response<ServeFileSystemResponseBody>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        <ServeDir<F> as Service<Request<ReqBody>>>::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let mut req = req;
        // Entry-point requests serve the generated `pkg/index.html`
        // (hashed bundle reference) when a build has produced one;
        // otherwise the source index.html resolves as usual. Checked
        // per request so a build landing under a running server takes
        // effect without a restart.
        if prefers_generated_index(&self.root, req.uri().path()) {
            *req.uri_mut() = Uri::from_static("/pkg/index.html");
        }
        // The hashed-name decision needs the request path; the HTML
        // decision needs the response content-type (an SPA fallback
        // serves index.html under a route-shaped path) — so the path
        // verdict is captured here and both are applied after the
        // inner service answered.
        let immutable = is_content_hashed_path(req.uri().path());
        let fut = self.inner.call(req);
        Box::pin(async move {
            let mut res = fut.await?;
            apply_cache_policy(immutable, &mut res);
            Ok(res)
        })
    }
}

fn apply_cache_policy(immutable: bool, res: &mut Response<ServeFileSystemResponseBody>) {
    if res.headers().contains_key(header::CACHE_CONTROL) {
        return;
    }
    if immutable && res.status().is_success() {
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(ASSET_CACHE_CONTROL),
        );
        return;
    }
    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    if is_html {
        res.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
}

/// True when the request path's last segment is the content-hashed
/// bundle pair `pocopine build` emits (`<stem>.<hash8>.js` / `.wasm`).
/// Delegates the name-shape test to [`pocopine_assets::is_hashed_bundle_name`]
/// so the dev server and this production path can never disagree.
fn is_content_hashed_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    is_hashed_bundle_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn request(path: &str) -> Request<Body> {
        Request::builder().uri(path).body(Body::empty()).unwrap()
    }

    fn cache_control(res: &Response<ServeFileSystemResponseBody>) -> Option<String> {
        res.headers()
            .get(header::CACHE_CONTROL)
            .map(|value| value.to_str().unwrap().to_string())
    }

    async fn body_string(res: Response<ServeFileSystemResponseBody>) -> String {
        let body = axum::body::Body::new(res.into_body());
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn content_hashed_path_matches_bundle_pair_only() {
        assert!(is_content_hashed_path("/pkg/website.0a1b2c3d.js"));
        assert!(is_content_hashed_path("/pkg/website_bg.0a1b2c3d.wasm"));
        // Unhashed pair, wrong hash shape, other extensions.
        assert!(!is_content_hashed_path("/pkg/website.js"));
        assert!(!is_content_hashed_path("/pkg/website_bg.wasm"));
        assert!(!is_content_hashed_path("/pkg/website.0A1B2C3D.js"));
        assert!(!is_content_hashed_path("/pkg/website.abc.js"));
        assert!(!is_content_hashed_path("/img/photo.0a1b2c3d.png"));
    }

    #[tokio::test]
    async fn hashed_bundle_is_immutable_and_html_revalidates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg/app.0a1b2c3d.js"), "glue").unwrap();
        std::fs::write(dir.path().join("pkg/app_bg.0a1b2c3d.wasm"), "wasm").unwrap();
        std::fs::write(dir.path().join("pkg/plain.js"), "js").unwrap();
        std::fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();

        let svc = StaticFiles::new(dir.path());

        let res = svc
            .clone()
            .oneshot(request("/pkg/app.0a1b2c3d.js"))
            .await
            .unwrap();
        assert_eq!(
            cache_control(&res).as_deref(),
            Some("public,max-age=31536000,immutable")
        );

        let res = svc
            .clone()
            .oneshot(request("/pkg/app_bg.0a1b2c3d.wasm"))
            .await
            .unwrap();
        assert_eq!(
            cache_control(&res).as_deref(),
            Some("public,max-age=31536000,immutable")
        );

        // index.html (and "/" which resolves to it) must revalidate.
        let res = svc.clone().oneshot(request("/index.html")).await.unwrap();
        assert_eq!(cache_control(&res).as_deref(), Some("no-cache"));
        let res = svc.clone().oneshot(request("/")).await.unwrap();
        assert_eq!(cache_control(&res).as_deref(), Some("no-cache"));

        // Unhashed, non-HTML files keep no explicit policy.
        let res = svc.clone().oneshot(request("/pkg/plain.js")).await.unwrap();
        assert_eq!(cache_control(&res), None);

        // A hashed name that 404s is not stamped immutable.
        let res = svc
            .clone()
            .oneshot(request("/pkg/gone.0a1b2c3d.js"))
            .await
            .unwrap();
        assert_eq!(cache_control(&res), None);
    }

    #[tokio::test]
    async fn entry_point_prefers_the_generated_pkg_index_when_built() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "source").unwrap();
        let svc = StaticFiles::new(dir.path());

        // No build output — the source index serves (dev without a
        // build still works).
        for path in ["/", "/index.html"] {
            let res = svc.clone().oneshot(request(path)).await.unwrap();
            assert_eq!(body_string(res).await, "source");
        }

        // `pocopine build` wrote the hash-rewritten copy — entry-point
        // requests flip to it without a restart, still `no-cache`.
        std::fs::create_dir(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg/index.html"), "generated").unwrap();
        for path in ["/", "/index.html"] {
            let res = svc.clone().oneshot(request(path)).await.unwrap();
            assert_eq!(cache_control(&res).as_deref(), Some("no-cache"));
            assert_eq!(body_string(res).await, "generated");
        }

        // Non-entry-point paths are not re-pointed.
        let res = svc.clone().oneshot(request("/index.htm")).await.unwrap();
        assert_ne!(body_string(res).await, "generated");
    }

    #[tokio::test]
    async fn spa_fallback_response_gets_no_cache() {
        use axum::handler::HandlerWithoutStateExt;
        use axum::response::Html;

        let dir = tempfile::tempdir().unwrap();
        let fallback = (|| async { Html("<!doctype html>") }).into_service();
        let svc = StaticFiles::new(dir.path()).fallback(fallback);

        let res = svc.oneshot(request("/docs/some/route")).await.unwrap();
        assert_eq!(cache_control(&res).as_deref(), Some("no-cache"));
    }
}
