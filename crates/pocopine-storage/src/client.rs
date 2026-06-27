use pocopine_core::{App, AppPlugin};

use crate::{
    STORAGE_ENDPOINT_PREFIX, STORAGE_TUS_ENDPOINT_PREFIX, SignedRead, StorageError,
    StorageObjectScope, StorageResult, UploadPolicyDescriptor, UploadSession, UploadSessionId,
};

/// App plugin that provides [`StorageClient`] to components.
#[derive(Clone, Debug)]
pub struct StorageClientPlugin {
    endpoint: String,
    with_credentials: bool,
}

impl Default for StorageClientPlugin {
    fn default() -> Self {
        Self {
            endpoint: STORAGE_ENDPOINT_PREFIX.to_string(),
            with_credentials: false,
        }
    }
}

/// Build the storage app plugin.
pub fn storage_plugin() -> StorageClientPlugin {
    StorageClientPlugin::default()
}

/// App plugin that provides [`UploadClient`] to components.
#[derive(Clone, Debug)]
pub struct UploadClientPlugin {
    endpoint: String,
    with_credentials: bool,
}

impl Default for UploadClientPlugin {
    fn default() -> Self {
        Self {
            endpoint: STORAGE_TUS_ENDPOINT_PREFIX.to_string(),
            with_credentials: false,
        }
    }
}

/// Build the browser upload app plugin.
pub fn upload_plugin() -> UploadClientPlugin {
    UploadClientPlugin::default()
}

impl UploadClientPlugin {
    /// Override the resumable-upload HTTP endpoint prefix.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Set browser credentials mode for upload fetches.
    pub fn with_credentials(mut self, enabled: bool) -> Self {
        self.with_credentials = enabled;
        self
    }

    /// Build a runtime client without mounting an app.
    pub fn into_client(self) -> UploadClient {
        UploadClient {
            endpoint: self.endpoint,
            with_credentials: self.with_credentials,
        }
    }
}

impl AppPlugin for UploadClientPlugin {
    fn name(&self) -> &'static str {
        "pocopine-storage-upload"
    }

    fn install(self, app: App) -> App {
        app.provide_plugin(self.into_client())
    }
}

impl StorageClientPlugin {
    /// Override the storage HTTP endpoint prefix.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Set browser credentials mode for storage fetches.
    pub fn with_credentials(mut self, enabled: bool) -> Self {
        self.with_credentials = enabled;
        self
    }

    /// Build a runtime client without mounting an app.
    pub fn into_client(self) -> StorageClient {
        StorageClient {
            endpoint: self.endpoint,
            with_credentials: self.with_credentials,
        }
    }
}

impl AppPlugin for StorageClientPlugin {
    fn name(&self) -> &'static str {
        "pocopine-storage"
    }

    fn install(self, app: App) -> App {
        app.provide_plugin(self.into_client())
    }
}

/// Runtime storage client service installed by [`storage_plugin`].
#[derive(Clone, Debug)]
pub struct StorageClient {
    endpoint: String,
    with_credentials: bool,
}

impl Default for StorageClient {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageClient {
    /// Build a client using the default storage endpoint.
    pub fn new() -> Self {
        StorageClientPlugin::default().into_client()
    }

    /// Override the endpoint on a direct client.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Set browser credentials mode on a direct client.
    pub fn with_credentials(mut self, enabled: bool) -> Self {
        self.with_credentials = enabled;
        self
    }

    /// Bind this client to a server-registered storage scope.
    pub fn scope(&self, scope: impl Into<String>) -> StorageScopeClient {
        StorageScopeClient {
            endpoint: self.endpoint.clone(),
            with_credentials: self.with_credentials,
            scope: scope.into(),
        }
    }

    /// Bind this client to a typed storage object scope.
    pub fn object_scope<S>(&self) -> StorageScopeClient
    where
        S: StorageObjectScope,
    {
        self.scope(S::NAME)
    }
}

/// Runtime browser upload client service installed by [`upload_plugin`].
#[derive(Clone, Debug)]
pub struct UploadClient {
    endpoint: String,
    with_credentials: bool,
}

impl Default for UploadClient {
    fn default() -> Self {
        Self::new()
    }
}

impl UploadClient {
    /// Build an upload client using the default storage upload endpoint.
    pub fn new() -> Self {
        UploadClientPlugin::default().into_client()
    }

    /// Override the endpoint on a direct client.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Set browser credentials mode on a direct client.
    pub fn with_credentials(mut self, enabled: bool) -> Self {
        self.with_credentials = enabled;
        self
    }

    /// Bind this client to a server-registered storage scope.
    pub fn scope(&self, scope: impl Into<String>) -> UploadScopeClient {
        UploadScopeClient {
            endpoint: self.endpoint.clone(),
            with_credentials: self.with_credentials,
            scope: scope.into(),
        }
    }

    /// Bind this upload client to a typed storage object scope.
    pub fn object_scope<S>(&self) -> UploadScopeClient
    where
        S: StorageObjectScope,
    {
        self.scope(S::NAME)
    }
}

/// Scope-bound browser upload client.
#[derive(Clone, Debug)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct UploadScopeClient {
    endpoint: String,
    with_credentials: bool,
    scope: String,
}

/// Result returned by the browser resumable upload client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumableUpload {
    pub upload_url: String,
    pub bytes_uploaded: u64,
}

/// Scope-bound storage client.
#[derive(Clone, Debug)]
pub struct StorageScopeClient {
    endpoint: String,
    with_credentials: bool,
    scope: String,
}

impl StorageScopeClient {
    /// Bind this scope client to a completed object key.
    pub fn object(&self, key: impl Into<String>) -> StorageObjectClient {
        StorageObjectClient {
            endpoint: self.endpoint.clone(),
            with_credentials: self.with_credentials,
            scope: self.scope.clone(),
            key: key.into(),
        }
    }
}

/// Client for a completed object inside a storage scope.
#[derive(Clone, Debug)]
pub struct StorageObjectClient {
    endpoint: String,
    with_credentials: bool,
    scope: String,
    key: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl StorageScopeClient {
    /// Host compile stub. Browser fetch support is only available on wasm32.
    pub async fn descriptor(&self) -> StorageResult<UploadPolicyDescriptor> {
        let _ = (&self.endpoint, self.with_credentials, &self.scope);
        Err(StorageError::unsupported(
            "storage client HTTP calls are only available in the browser runtime",
        ))
    }

    /// Host compile stub. Browser fetch support is only available on wasm32.
    pub async fn session(&self, id: UploadSessionId) -> StorageResult<UploadSession> {
        let _ = (&self.endpoint, self.with_credentials, &self.scope, id);
        Err(StorageError::unsupported(
            "storage client HTTP calls are only available in the browser runtime",
        ))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl StorageObjectClient {
    /// Host compile stub. Browser fetch support is only available on wasm32.
    pub async fn signed_read(&self) -> StorageResult<SignedRead> {
        let _ = (
            &self.endpoint,
            self.with_credentials,
            &self.scope,
            &self.key,
        );
        Err(StorageError::unsupported(
            "storage client HTTP calls are only available in the browser runtime",
        ))
    }

    /// Host compile stub. Browser fetch support is only available on wasm32.
    pub async fn download_url(&self) -> StorageResult<String> {
        Ok(self.signed_read().await?.url)
    }

    /// Host compile stub. Browser fetch support is only available on wasm32.
    pub async fn delete(&self) -> StorageResult<()> {
        let _ = (
            &self.endpoint,
            self.with_credentials,
            &self.scope,
            &self.key,
        );
        Err(StorageError::unsupported(
            "storage client HTTP calls are only available in the browser runtime",
        ))
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct UploadBuilder;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct ResumableUploadBuilder;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::Cell;
    #[cfg(any(test, feature = "test-utils"))]
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;

    use futures_util::stream::{FuturesUnordered, StreamExt as _};
    use js_sys::Promise;
    use serde::de::DeserializeOwned;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{
        AbortSignal, Blob, File, Headers, Request, RequestCredentials, RequestInit, Response,
    };

    use super::*;
    use crate::{
        CompleteUploadRequest, InitiateUploadRequest, ObjectRef, PartSpec, ReadUrlRequest,
        STORAGE_PROTOCOL_V1, StorageResponse, UploadPhase, UploadProgress, UploadStrategy,
        plan_parts,
    };

    const DEFAULT_CHUNK_SIZE: u64 = 1024 * 1024;
    const TUS_PROTOCOL_VERSION: &str = "1.0.0";
    /// Hard ceiling on in-flight multipart part uploads, regardless of what the
    /// server's plan advertises. Browsers cap concurrent connections per origin
    /// anyway; this just keeps a buggy/hostile plan from making us seed an
    /// unbounded sliding window of fetches.
    const MAX_CLIENT_CONCURRENT_PARTS: usize = 16;

    /// Test hook for the browser upload transport.
    #[doc(hidden)]
    pub trait BrowserStorageTransport {
        fn request(
            &self,
            request: BrowserStorageRequest,
        ) -> Pin<Box<dyn Future<Output = StorageResult<BrowserStorageResponse>>>>;
    }

    /// Test-visible browser transport request.
    #[doc(hidden)]
    #[derive(Clone)]
    pub struct BrowserStorageRequest {
        pub method: String,
        pub url: String,
        pub headers: Vec<(String, String)>,
        pub json_body: Option<String>,
        pub blob_body: Option<Blob>,
        pub with_credentials: bool,
        pub abort_signal: Option<AbortSignal>,
    }

    /// Test-visible browser transport response.
    #[doc(hidden)]
    #[derive(Clone, Debug)]
    pub struct BrowserStorageResponse {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    impl BrowserStorageResponse {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    thread_local! {
        static TEST_TRANSPORT: RefCell<Option<Rc<dyn BrowserStorageTransport>>> =
            RefCell::new(None);
    }

    /// Install a fake transport for browser test harnesses. Gated behind the
    /// `test-utils` feature so it cannot be reached from production bundles —
    /// otherwise any in-bundle JS holding the wasm exports could MITM every
    /// storage HTTP call (including upload payloads).
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn __set_browser_transport_for_test<T>(transport: T)
    where
        T: BrowserStorageTransport + 'static,
    {
        TEST_TRANSPORT.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(transport));
        });
    }

    /// Clear any installed fake transport. See `__set_browser_transport_for_test`.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn __reset_browser_transport_for_test() {
        TEST_TRANSPORT.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }

    impl super::StorageScopeClient {
        /// Fetch the safe upload policy descriptor for this scope.
        pub async fn descriptor(&self) -> StorageResult<UploadPolicyDescriptor> {
            fetch_json(
                BrowserStorageRequest::get(
                    scope_url(&self.endpoint, &self.scope),
                    self.with_credentials,
                ),
                "scope descriptor",
            )
            .await
        }

        /// Inspect a resumable upload session.
        pub async fn session(&self, id: UploadSessionId) -> StorageResult<UploadSession> {
            fetch_json(
                BrowserStorageRequest::get(
                    upload_url(&self.endpoint, id.as_str()),
                    self.with_credentials,
                ),
                "upload session",
            )
            .await
        }

        /// Start an upload from a browser [`File`].
        pub fn upload(&self, file: File) -> UploadBuilder {
            let name = file.name();
            let size = file.size() as u64;
            let content_type = empty_to_none(file.type_());
            let last_modified = Some(file.last_modified() as i64);
            let blob: Blob = file.unchecked_into();
            self.upload_source(UploadSource {
                blob,
                name,
                size,
                content_type,
                last_modified,
            })
        }

        /// Start an upload from a browser [`Blob`] with an app-provided name.
        pub fn upload_blob(&self, blob: Blob, name: impl Into<String>) -> UploadBuilder {
            let size = blob.size() as u64;
            let content_type = empty_to_none(blob.type_());
            self.upload_source(UploadSource {
                blob,
                name: name.into(),
                size,
                content_type,
                last_modified: None,
            })
        }

        /// Resume an existing browser-safe upload session.
        pub fn resume(&self, file: File, session: UploadSession) -> UploadBuilder {
            self.upload(file).session(session)
        }

        fn upload_source(&self, source: UploadSource) -> UploadBuilder {
            UploadBuilder {
                endpoint: self.endpoint.clone(),
                with_credentials: self.with_credentials,
                scope: self.scope.clone(),
                source,
                strategy: UploadStrategy::Auto,
                metadata: BTreeMap::new(),
                progress: None,
                retry_limit: 2,
                retry_base_delay_ms: 100,
                abort_signal: None,
                resume_session: None,
                auto_resume: false,
            }
        }
    }

    impl super::StorageObjectClient {
        /// Ask the server for a short-lived URL that reads this object.
        pub async fn signed_read(&self) -> StorageResult<SignedRead> {
            fetch_json(
                BrowserStorageRequest::post_json(
                    object_read_url(&self.endpoint, &self.scope, &self.key),
                    &ReadUrlRequest::default(),
                    self.with_credentials,
                    None,
                )?,
                "object read url",
            )
            .await
        }

        /// Return the URL portion of [`Self::signed_read`].
        pub async fn download_url(&self) -> StorageResult<String> {
            Ok(self.signed_read().await?.url)
        }

        /// Delete this completed object.
        pub async fn delete(&self) -> StorageResult<()> {
            fetch_no_content(
                BrowserStorageRequest::delete(
                    object_url(&self.endpoint, &self.scope, &self.key),
                    self.with_credentials,
                    None,
                ),
                "delete object",
            )
            .await
        }
    }

    impl super::UploadScopeClient {
        /// Start a resumable upload from a browser [`File`].
        pub fn upload(&self, file: File) -> ResumableUploadBuilder {
            let name = file.name();
            let size = file.size() as u64;
            let content_type = empty_to_none(file.type_());
            let last_modified = Some(file.last_modified() as i64);
            let blob: Blob = file.unchecked_into();
            self.upload_source(UploadSource {
                blob,
                name,
                size,
                content_type,
                last_modified,
            })
        }

        /// Start a resumable upload from a browser [`Blob`] with an app-provided name.
        pub fn upload_blob(&self, blob: Blob, name: impl Into<String>) -> ResumableUploadBuilder {
            let size = blob.size() as u64;
            let content_type = empty_to_none(blob.type_());
            self.upload_source(UploadSource {
                blob,
                name: name.into(),
                size,
                content_type,
                last_modified: None,
            })
        }

        /// Resume an upload from a known upload URL.
        pub fn resume(&self, file: File, upload_url: impl Into<String>) -> ResumableUploadBuilder {
            self.upload(file).upload_url(upload_url)
        }

        fn upload_source(&self, source: UploadSource) -> ResumableUploadBuilder {
            ResumableUploadBuilder {
                endpoint: self.endpoint.clone(),
                with_credentials: self.with_credentials,
                scope: self.scope.clone(),
                source,
                metadata: BTreeMap::new(),
                progress: None,
                retry_limit: 2,
                retry_base_delay_ms: 100,
                abort_signal: None,
                upload_url: None,
                auto_resume: false,
                chunk_size: DEFAULT_CHUNK_SIZE,
            }
        }
    }

    #[derive(Clone)]
    struct UploadSource {
        blob: Blob,
        name: String,
        size: u64,
        content_type: Option<String>,
        last_modified: Option<i64>,
    }

    /// Browser upload builder.
    #[must_use]
    pub struct UploadBuilder {
        endpoint: String,
        with_credentials: bool,
        scope: String,
        source: UploadSource,
        strategy: UploadStrategy,
        metadata: BTreeMap<String, String>,
        progress: Option<Rc<dyn Fn(UploadProgress)>>,
        retry_limit: u32,
        retry_base_delay_ms: u32,
        abort_signal: Option<AbortSignal>,
        resume_session: Option<UploadSession>,
        auto_resume: bool,
    }

    impl UploadBuilder {
        /// Set the requested upload strategy.
        pub fn strategy(mut self, strategy: UploadStrategy) -> Self {
            self.strategy = strategy;
            self
        }

        /// Add string metadata to the upload initiation request.
        pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
            self.metadata.insert(key.into(), value.into());
            self
        }

        /// Observe upload progress.
        pub fn on_progress<F>(mut self, callback: F) -> Self
        where
            F: Fn(UploadProgress) + 'static,
        {
            self.progress = Some(Rc::new(callback));
            self
        }

        /// Set the number of retries for transient browser transport errors.
        pub fn retry_limit(mut self, retry_limit: u32) -> Self {
            self.retry_limit = retry_limit;
            self
        }

        /// Set the base retry backoff in milliseconds.
        pub fn retry_base_delay_ms(mut self, retry_base_delay_ms: u32) -> Self {
            self.retry_base_delay_ms = retry_base_delay_ms;
            self
        }

        /// Attach a browser abort signal to every request in this upload.
        pub fn abort_signal(mut self, signal: AbortSignal) -> Self {
            self.abort_signal = Some(signal);
            self
        }

        /// Continue from an existing session instead of creating a new one.
        pub fn session(mut self, session: UploadSession) -> Self {
            self.resume_session = Some(session);
            self
        }

        /// Opt into browser localStorage session lookup for this source.
        ///
        /// This is disabled by default so same-name/same-size files never resume
        /// implicitly into an unrelated upload session.
        pub fn auto_resume(mut self, enabled: bool) -> Self {
            self.auto_resume = enabled;
            self
        }

        /// Upload the file/blob through the sequential proxy route.
        pub async fn send(self) -> StorageResult<ObjectRef> {
            self.emit(UploadPhase::Initiating, 0);

            let session = match self.resume_session.clone() {
                Some(session) => session,
                None if self.auto_resume => {
                    if let Some(session) = self.load_resume_session().await? {
                        session
                    } else {
                        self.initiate().await?
                    }
                }
                None => self.initiate().await?,
            };

            // The server negotiated the strategy at initiate; the client drives the
            // matching transport. SingleRequest/Auto are not client-driven. Each
            // path owns its own resume-session persistence (only the sequential
            // path is offset-resumable).
            match session.strategy {
                UploadStrategy::Sequential => self.send_sequential(session).await,
                UploadStrategy::Multipart => self.send_multipart(session).await,
                other => {
                    self.emit(UploadPhase::Failed, session.next_offset.unwrap_or(0));
                    Err(StorageError::unsupported(format!(
                        "the browser client cannot drive the {other:?} upload strategy"
                    )))
                }
            }
        }

        /// Sequential proxy upload: ordered `PATCH` chunks coalesced server-side.
        async fn send_sequential(&self, mut session: UploadSession) -> StorageResult<ObjectRef> {
            self.store_resume_session(&session);
            let mut offset = self.inspect_offset(&session).await?;
            while offset < self.source.size {
                self.emit(UploadPhase::Uploading, offset);
                let end = offset
                    .saturating_add(self.chunk_size(&session))
                    .min(self.source.size);
                let chunk = self.slice(offset, end)?;
                let mut attempts = 0;
                loop {
                    match self.patch_chunk(&session, offset, chunk.clone()).await {
                        Ok(updated) => {
                            session = updated;
                            self.store_resume_session(&session);
                            offset = session.next_offset.unwrap_or(end);
                            self.emit(UploadPhase::Uploading, offset);
                            break;
                        }
                        Err(
                            StorageError::OffsetMismatch { .. } | StorageError::Conflict { .. },
                        ) => {
                            session = self.inspect_session(&session).await?;
                            self.store_resume_session(&session);
                            offset = session.next_offset.unwrap_or(0);
                            break;
                        }
                        Err(err)
                            if err.is_retryable_client_error()
                                && attempts < self.retry_limit
                                && !self.is_aborted() =>
                        {
                            attempts += 1;
                            self.emit(UploadPhase::Retrying, offset);
                            retry_delay(self.retry_base_delay_ms, attempts).await?;
                        }
                        Err(err) => {
                            self.emit(UploadPhase::Failed, offset);
                            return Err(err);
                        }
                    }
                }
            }

            self.emit(UploadPhase::Completing, self.source.size);
            let object = self.complete_retrying(&session).await?;
            self.clear_resume_session();
            self.emit(UploadPhase::Complete, self.source.size);
            Ok(object)
        }

        /// Server-mediated multipart upload: parts (sized by the session plan) are
        /// `PUT` to the by-number part endpoint with bounded concurrency, then the
        /// upload is completed (the server assembles from the provider's parts).
        async fn send_multipart(&self, session: UploadSession) -> StorageResult<ObjectRef> {
            let parts = plan_parts(self.source.size, &session.plan)?;
            // Clamp the server-advertised concurrency to a client ceiling so a
            // buggy/hostile plan can't make us seed thousands of in-flight part
            // fetches at once.
            let concurrency = usize::from(session.plan.max_concurrent_parts.max(1))
                .min(MAX_CLIENT_CONCURRENT_PARTS);
            let total = self.source.size;
            // Aggregate completed bytes, shared with the part futures so a
            // per-part `Retrying` reports the true monotonic total (parts complete
            // out of order, so a part's own offset could jump ahead then regress).
            let uploaded = Rc::new(Cell::new(0u64));
            let mut specs = parts.into_iter();
            // Sliding window of in-flight parts, capped at the advertised
            // concurrency. wasm is single-threaded, so these futures are `!Send`
            // and run on this one task — no `spawn_local`.
            let mut in_flight = FuturesUnordered::new();
            for _ in 0..concurrency {
                if let Some(spec) = specs.next() {
                    in_flight.push(self.upload_part(&session, spec, uploaded.clone()));
                }
            }
            while let Some(result) = in_flight.next().await {
                match result {
                    Ok(spec) => {
                        uploaded.set(uploaded.get().saturating_add(spec.len));
                        self.emit_part(UploadPhase::Uploading, uploaded.get(), spec.number);
                        if let Some(spec) = specs.next() {
                            in_flight.push(self.upload_part(&session, spec, uploaded.clone()));
                        }
                    }
                    Err(err) => {
                        self.emit(UploadPhase::Failed, uploaded.get());
                        // Best-effort cleanup. Sibling part PUTs still in flight are
                        // not individually cancelled, but the server's abort
                        // reconciles them: a late part either hits the now-deleted
                        // session (rejected) or lands a provider part the abort's
                        // own cleanup removes (S3 NoSuchUpload, GCS component-range
                        // delete, Azure uncommitted-block GC).
                        let _ = self.abort(&session).await;
                        return Err(err);
                    }
                }
            }

            self.emit(UploadPhase::Completing, total);
            // Retry a transient failure on the final assemble (re-complete is
            // idempotent server-side); on a terminal failure clean up the
            // provider's multipart state rather than orphaning it.
            match self.complete_retrying(&session).await {
                Ok(object) => {
                    self.clear_resume_session();
                    self.emit(UploadPhase::Complete, total);
                    Ok(object)
                }
                Err(err) => {
                    self.emit(UploadPhase::Failed, total);
                    let _ = self.abort(&session).await;
                    Err(err)
                }
            }
        }

        /// Upload one part, retrying transient transport errors. Re-`PUT`ting the
        /// same part number is idempotent server-side, so a retry is safe. Returns
        /// the spec so the caller can advance progress.
        async fn upload_part(
            &self,
            session: &UploadSession,
            spec: PartSpec,
            uploaded: Rc<Cell<u64>>,
        ) -> StorageResult<PartSpec> {
            let blob = self.slice(spec.offset, spec.offset + spec.len)?;
            let mut attempts = 0;
            loop {
                match self.put_part(session, spec.number, blob.clone()).await {
                    Ok(()) => return Ok(spec),
                    Err(err)
                        if err.is_retryable_client_error()
                            && attempts < self.retry_limit
                            && !self.is_aborted() =>
                    {
                        attempts += 1;
                        // Report the aggregate completed bytes (monotonic), not this
                        // part's offset, so progress never jumps ahead/regresses.
                        self.emit_part(UploadPhase::Retrying, uploaded.get(), spec.number);
                        retry_delay(self.retry_base_delay_ms, attempts).await?;
                    }
                    Err(err) => return Err(err),
                }
            }
        }

        async fn put_part(
            &self,
            session: &UploadSession,
            number: u32,
            blob: Blob,
        ) -> StorageResult<()> {
            // The server records the part on the provider and returns the session;
            // we only need success — completion lists the parts server-side, so the
            // client never reports per-part receipts on the proxy path. Check the
            // status only rather than deserializing (and discarding) the session
            // body, so the part path doesn't break if the server ever answers a
            // part PUT with an empty/2xx body.
            fetch_no_content(
                BrowserStorageRequest::put_part(
                    upload_url(&self.endpoint, session.id.as_str()),
                    number,
                    blob,
                    self.with_credentials,
                    self.abort_signal.clone(),
                ),
                "upload part",
            )
            .await
        }

        async fn abort(&self, session: &UploadSession) -> StorageResult<()> {
            // Deliberately omit the caller's `AbortSignal`: this cleanup often runs
            // *because* that signal fired (user cancel / timeout), and a
            // pre-aborted signal makes `fetch` reject before sending — so the
            // server would never receive the DELETE and the provider's multipart
            // state would leak until expiry.
            send_request(BrowserStorageRequest::delete(
                upload_url(&self.endpoint, session.id.as_str()),
                self.with_credentials,
                None,
            ))
            .await
            .map(|_| ())
        }

        async fn initiate(&self) -> StorageResult<UploadSession> {
            let request = InitiateUploadRequest {
                protocol: STORAGE_PROTOCOL_V1.to_string(),
                scope: self.scope.clone(),
                file_name: self.source.name.clone(),
                size: Some(self.source.size),
                content_type: self.source.content_type.clone(),
                metadata: self.metadata.clone(),
                requested_strategy: self.strategy,
            };
            fetch_json(
                BrowserStorageRequest::post_json(
                    uploads_url(&self.endpoint),
                    &request,
                    self.with_credentials,
                    self.abort_signal.clone(),
                )?,
                "initiate upload",
            )
            .await
        }

        async fn inspect_session(&self, session: &UploadSession) -> StorageResult<UploadSession> {
            fetch_json(
                BrowserStorageRequest::get(
                    upload_url(&self.endpoint, session.id.as_str()),
                    self.with_credentials,
                )
                .abort_signal(self.abort_signal.clone()),
                "inspect upload",
            )
            .await
        }

        async fn inspect_offset(&self, session: &UploadSession) -> StorageResult<u64> {
            let session = self.inspect_session(session).await?;
            Ok(session.next_offset.unwrap_or(0))
        }

        async fn patch_chunk(
            &self,
            session: &UploadSession,
            offset: u64,
            chunk: Blob,
        ) -> StorageResult<UploadSession> {
            fetch_json(
                BrowserStorageRequest::patch_blob(
                    // Sequential append PATCHes the session resource directly
                    // (TUS-shaped); the part number / offset rides in a header.
                    upload_url(&self.endpoint, session.id.as_str()),
                    offset,
                    chunk,
                    self.with_credentials,
                    self.abort_signal.clone(),
                ),
                "upload bytes",
            )
            .await
        }

        async fn complete(&self, session: &UploadSession) -> StorageResult<ObjectRef> {
            fetch_json(
                BrowserStorageRequest::post_json(
                    upload_complete_url(&self.endpoint, session.id.as_str()),
                    &CompleteUploadRequest::default(),
                    self.with_credentials,
                    self.abort_signal.clone(),
                )?,
                "complete upload",
            )
            .await
        }

        /// Complete, retrying transient transport errors. Re-completing is
        /// idempotent server-side (the server lists the provider's parts and a
        /// second assemble returns the same `ObjectRef`), so a retry is safe and
        /// keeps a flaky final request from discarding a fully-uploaded object.
        async fn complete_retrying(&self, session: &UploadSession) -> StorageResult<ObjectRef> {
            let mut attempts = 0;
            loop {
                match self.complete(session).await {
                    Ok(object) => return Ok(object),
                    Err(err)
                        if err.is_retryable_client_error()
                            && attempts < self.retry_limit
                            && !self.is_aborted() =>
                    {
                        attempts += 1;
                        retry_delay(self.retry_base_delay_ms, attempts).await?;
                    }
                    Err(err) => return Err(err),
                }
            }
        }

        fn chunk_size(&self, session: &UploadSession) -> u64 {
            session
                .plan
                .preferred_part_size
                .or(session.part_size)
                .unwrap_or(DEFAULT_CHUNK_SIZE)
                .max(1)
        }

        fn slice(&self, start: u64, end: u64) -> StorageResult<Blob> {
            self.source
                .blob
                .slice_with_f64_and_f64(start as f64, end as f64)
                .map_err(|err| StorageError::client(format!("slice upload blob: {err:?}")))
        }

        fn emit(&self, phase: UploadPhase, bytes_sent: u64) {
            self.emit_progress(phase, bytes_sent, None);
        }

        fn emit_part(&self, phase: UploadPhase, bytes_sent: u64, number: u32) {
            self.emit_progress(phase, bytes_sent, Some(number));
        }

        fn emit_progress(&self, phase: UploadPhase, bytes_sent: u64, current_part: Option<u32>) {
            if let Some(callback) = &self.progress {
                callback(UploadProgress {
                    bytes_sent,
                    bytes_total: Some(self.source.size),
                    current_part,
                    phase,
                });
            }
        }

        /// True once the caller's `AbortSignal` has fired. Used to stop retrying
        /// a transient transport error when the upload has been cancelled (a
        /// cancelled `fetch` rejects as a retryable client error, so without this
        /// guard the part/chunk loops would spin against a dead signal).
        fn is_aborted(&self) -> bool {
            self.abort_signal
                .as_ref()
                .is_some_and(web_sys::AbortSignal::aborted)
        }

        async fn load_resume_session(&self) -> StorageResult<Option<UploadSession>> {
            let Some(session) = read_resume_session(&self.resume_key()) else {
                return Ok(None);
            };
            match self.inspect_session(&session).await {
                Ok(session) if session.status == crate::UploadSessionStatus::Open => {
                    Ok(Some(session))
                }
                Ok(_) | Err(StorageError::UnknownUploadSession { .. }) => {
                    self.clear_resume_session();
                    Ok(None)
                }
                Err(err) => Err(err),
            }
        }

        fn store_resume_session(&self, session: &UploadSession) {
            write_resume_session(&self.resume_key(), session);
        }

        fn clear_resume_session(&self) {
            remove_resume_session(&self.resume_key());
        }

        fn resume_key(&self) -> String {
            format!(
                "pocopine.storage.resume.v1:{}:{}:{}:{}",
                self.scope,
                self.source.name,
                self.source.size,
                self.source
                    .last_modified
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "blob".to_string())
            )
        }
    }

    /// Browser resumable upload builder.
    #[must_use]
    pub struct ResumableUploadBuilder {
        endpoint: String,
        with_credentials: bool,
        scope: String,
        source: UploadSource,
        metadata: BTreeMap<String, String>,
        progress: Option<Rc<dyn Fn(UploadProgress)>>,
        retry_limit: u32,
        retry_base_delay_ms: u32,
        abort_signal: Option<AbortSignal>,
        upload_url: Option<String>,
        auto_resume: bool,
        chunk_size: u64,
    }

    struct UploadHead {
        offset: u64,
        length: Option<u64>,
    }

    enum PatchOutcome {
        Advanced(u64),
        Conflict,
    }

    impl ResumableUploadBuilder {
        /// Add string metadata to the upload creation request.
        pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
            self.metadata.insert(key.into(), value.into());
            self
        }

        /// Observe upload progress.
        pub fn on_progress<F>(mut self, callback: F) -> Self
        where
            F: Fn(UploadProgress) + 'static,
        {
            self.progress = Some(Rc::new(callback));
            self
        }

        /// Set the number of retries for transient browser transport errors.
        pub fn retry_limit(mut self, retry_limit: u32) -> Self {
            self.retry_limit = retry_limit;
            self
        }

        /// Set the base retry backoff in milliseconds.
        pub fn retry_base_delay_ms(mut self, retry_base_delay_ms: u32) -> Self {
            self.retry_base_delay_ms = retry_base_delay_ms;
            self
        }

        /// Attach a browser abort signal to every upload request.
        pub fn abort_signal(mut self, signal: AbortSignal) -> Self {
            self.abort_signal = Some(signal);
            self
        }

        /// Resume from an existing upload URL.
        pub fn upload_url(mut self, upload_url: impl Into<String>) -> Self {
            self.upload_url = Some(upload_url.into());
            self
        }

        /// Opt into browser localStorage upload URL lookup for this source.
        ///
        /// This is disabled by default so same-name/same-size files never resume
        /// implicitly into an unrelated upload.
        pub fn auto_resume(mut self, enabled: bool) -> Self {
            self.auto_resume = enabled;
            self
        }

        /// Set the resumable upload PATCH chunk size in bytes.
        pub fn chunk_size(mut self, chunk_size: u64) -> Self {
            self.chunk_size = chunk_size.max(1);
            self
        }

        /// Upload the file/blob through the resumable creation and PATCH flow.
        pub async fn send(self) -> StorageResult<ResumableUpload> {
            self.emit(UploadPhase::Initiating, 0);

            let explicit_resume = self.upload_url.is_some();
            let resume_url = self.upload_url.clone().or_else(|| {
                self.auto_resume
                    .then(|| read_upload_resume_url(&self.resume_key()))
                    .flatten()
            });
            let (upload_url, mut offset) = if let Some(upload_url) = resume_url {
                match self.head_upload(&upload_url).await {
                    Ok(head)
                        if self.auto_resume
                            && !explicit_resume
                            && head.offset >= self.source.size =>
                    {
                        // Auto-resume key matched a session that is already
                        // complete on the server. The cache key only covers
                        // (name, size, last_modified), so identical metadata
                        // with different content (e.g. an in-place overwrite
                        // that preserves mtime) would otherwise silently
                        // return the stale ObjectRef. Start fresh.
                        self.validate_head_length(&head)?;
                        remove_upload_resume_url(&self.resume_key());
                        let created = self.create_upload().await?;
                        write_upload_resume_url(&self.resume_key(), &created.0);
                        created
                    }
                    Ok(head) => {
                        self.validate_head_length(&head)?;
                        (upload_url, head.offset)
                    }
                    Err(StorageError::UnknownUploadSession { .. })
                        if self.auto_resume && !explicit_resume =>
                    {
                        // Server confirmed the cached session is gone (404 /
                        // 410). Transient failures (5xx, network) propagate
                        // instead so we do not wipe a still-valid resume URL
                        // and force a full re-upload.
                        remove_upload_resume_url(&self.resume_key());
                        let created = self.create_upload().await?;
                        write_upload_resume_url(&self.resume_key(), &created.0);
                        created
                    }
                    Err(err) => return Err(err),
                }
            } else {
                let created = self.create_upload().await?;
                if self.auto_resume {
                    write_upload_resume_url(&self.resume_key(), &created.0);
                }
                created
            };
            if self.auto_resume {
                write_upload_resume_url(&self.resume_key(), &upload_url);
            }

            while offset < self.source.size {
                self.emit(UploadPhase::Uploading, offset);
                let end = offset.saturating_add(self.chunk_size).min(self.source.size);
                let chunk = self.slice(offset, end)?;
                let mut attempts = 0;
                loop {
                    match self.patch_chunk(&upload_url, offset, chunk.clone()).await {
                        Ok(PatchOutcome::Advanced(next_offset)) => {
                            offset = next_offset;
                            self.emit(UploadPhase::Uploading, offset);
                            break;
                        }
                        Ok(PatchOutcome::Conflict) => {
                            let head = self.head_upload(&upload_url).await?;
                            self.validate_head_length(&head)?;
                            offset = head.offset;
                            break;
                        }
                        Err(err)
                            if err.is_retryable_client_error() && attempts < self.retry_limit =>
                        {
                            attempts += 1;
                            self.emit(UploadPhase::Retrying, offset);
                            retry_delay(self.retry_base_delay_ms, attempts).await?;
                        }
                        Err(err) => {
                            self.emit(UploadPhase::Failed, offset);
                            return Err(err);
                        }
                    }
                }
            }

            self.emit(UploadPhase::Complete, offset);
            if self.auto_resume {
                remove_upload_resume_url(&self.resume_key());
            }
            Ok(ResumableUpload {
                upload_url,
                bytes_uploaded: offset,
            })
        }

        async fn create_upload(&self) -> StorageResult<(String, u64)> {
            let create_url = tus_uploads_url(&self.endpoint, &self.scope);
            let response = send_request(BrowserStorageRequest::tus_create(
                create_url.clone(),
                self.source.size,
                self.tus_metadata_header()?,
                self.with_credentials,
                self.abort_signal.clone(),
            ))
            .await?;
            ensure_tus_status(&response, "create upload", &[201])?;
            let location = response
                .header("location")
                .ok_or_else(|| StorageError::client("create upload omitted Location"))?;
            let upload_url = resolve_tus_location(&create_url, location)?;
            let offset = response
                .header("upload-offset")
                .map(parse_upload_offset_header)
                .transpose()?
                .unwrap_or(0);
            Ok((upload_url, offset))
        }

        async fn head_upload(&self, upload_url: &str) -> StorageResult<UploadHead> {
            let response = send_request(BrowserStorageRequest::head(
                upload_url.to_string(),
                self.with_credentials,
                self.abort_signal.clone(),
            ))
            .await?;
            ensure_tus_status(&response, "inspect upload", &[200, 204])?;
            let offset = response
                .header("upload-offset")
                .ok_or_else(|| StorageError::client("inspect upload omitted Upload-Offset"))
                .and_then(parse_upload_offset_header)?;
            let length = response
                .header("upload-length")
                .map(parse_upload_length_header)
                .transpose()?;
            Ok(UploadHead { offset, length })
        }

        async fn patch_chunk(
            &self,
            upload_url: &str,
            offset: u64,
            chunk: Blob,
        ) -> StorageResult<PatchOutcome> {
            let response = send_request(BrowserStorageRequest::tus_patch(
                upload_url.to_string(),
                offset,
                chunk,
                self.with_credentials,
                self.abort_signal.clone(),
            ))
            .await?;
            if response.status == 409 {
                return Ok(PatchOutcome::Conflict);
            }
            ensure_tus_status(&response, "patch upload", &[204])?;
            let next_offset = response
                .header("upload-offset")
                .ok_or_else(|| StorageError::client("patch upload omitted Upload-Offset"))
                .and_then(parse_upload_offset_header)?;
            Ok(PatchOutcome::Advanced(next_offset))
        }

        fn validate_head_length(&self, head: &UploadHead) -> StorageResult<()> {
            if let Some(length) = head.length
                && length != self.source.size
            {
                return Err(StorageError::client(format!(
                    "upload length mismatch: remote {length}, local {}",
                    self.source.size
                )));
            }
            Ok(())
        }

        fn tus_metadata_header(&self) -> StorageResult<Option<String>> {
            let mut metadata = self.metadata.clone();
            metadata
                .entry("filename".to_string())
                .or_insert_with(|| self.source.name.clone());
            if let Some(content_type) = &self.source.content_type {
                metadata
                    .entry("filetype".to_string())
                    .or_insert_with(|| content_type.clone());
            }
            encode_tus_metadata(&metadata)
        }

        fn slice(&self, start: u64, end: u64) -> StorageResult<Blob> {
            self.source
                .blob
                .slice_with_f64_and_f64(start as f64, end as f64)
                .map_err(|err| StorageError::client(format!("slice upload blob: {err:?}")))
        }

        fn emit(&self, phase: UploadPhase, bytes_sent: u64) {
            if let Some(callback) = &self.progress {
                callback(UploadProgress {
                    bytes_sent,
                    bytes_total: Some(self.source.size),
                    current_part: None,
                    phase,
                });
            }
        }

        fn resume_key(&self) -> String {
            format!(
                "pocopine.storage.upload.resume.v1:{}:{}:{}:{}",
                self.scope,
                self.source.name,
                self.source.size,
                self.source
                    .last_modified
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "blob".to_string())
            )
        }
    }

    impl BrowserStorageRequest {
        fn get(url: String, with_credentials: bool) -> Self {
            Self {
                method: "GET".to_string(),
                url,
                headers: Vec::new(),
                json_body: None,
                blob_body: None,
                with_credentials,
                abort_signal: None,
            }
        }

        fn head(url: String, with_credentials: bool, abort_signal: Option<AbortSignal>) -> Self {
            Self {
                method: "HEAD".to_string(),
                url,
                headers: vec![(
                    "Tus-Resumable".to_string(),
                    TUS_PROTOCOL_VERSION.to_string(),
                )],
                json_body: None,
                blob_body: None,
                with_credentials,
                abort_signal,
            }
        }

        fn post_json<T>(
            url: String,
            body: &T,
            with_credentials: bool,
            abort_signal: Option<AbortSignal>,
        ) -> StorageResult<Self>
        where
            T: serde::Serialize,
        {
            Ok(Self {
                method: "POST".to_string(),
                url,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                json_body: Some(
                    serde_json::to_string(body)
                        .map_err(|err| StorageError::client(format!("serialize request: {err}")))?,
                ),
                blob_body: None,
                with_credentials,
                abort_signal,
            })
        }

        fn tus_create(
            url: String,
            upload_length: u64,
            upload_metadata: Option<String>,
            with_credentials: bool,
            abort_signal: Option<AbortSignal>,
        ) -> Self {
            let mut headers = vec![
                (
                    "Tus-Resumable".to_string(),
                    TUS_PROTOCOL_VERSION.to_string(),
                ),
                ("Upload-Length".to_string(), upload_length.to_string()),
            ];
            if let Some(upload_metadata) = upload_metadata {
                headers.push(("Upload-Metadata".to_string(), upload_metadata));
            }
            Self {
                method: "POST".to_string(),
                url,
                headers,
                json_body: None,
                blob_body: None,
                with_credentials,
                abort_signal,
            }
        }

        fn patch_blob(
            url: String,
            offset: u64,
            blob: Blob,
            with_credentials: bool,
            abort_signal: Option<AbortSignal>,
        ) -> Self {
            Self {
                method: "PATCH".to_string(),
                url,
                headers: vec![("Upload-Offset".to_string(), offset.to_string())],
                json_body: None,
                blob_body: Some(blob),
                with_credentials,
                abort_signal,
            }
        }

        fn put_part(
            url: String,
            number: u32,
            blob: Blob,
            with_credentials: bool,
            abort_signal: Option<AbortSignal>,
        ) -> Self {
            Self {
                method: "PUT".to_string(),
                url,
                headers: vec![("Upload-Part".to_string(), number.to_string())],
                json_body: None,
                blob_body: Some(blob),
                with_credentials,
                abort_signal,
            }
        }

        fn delete(url: String, with_credentials: bool, abort_signal: Option<AbortSignal>) -> Self {
            Self {
                method: "DELETE".to_string(),
                url,
                headers: Vec::new(),
                json_body: None,
                blob_body: None,
                with_credentials,
                abort_signal,
            }
        }

        fn tus_patch(
            url: String,
            offset: u64,
            blob: Blob,
            with_credentials: bool,
            abort_signal: Option<AbortSignal>,
        ) -> Self {
            Self {
                method: "PATCH".to_string(),
                url,
                headers: vec![
                    (
                        "Tus-Resumable".to_string(),
                        TUS_PROTOCOL_VERSION.to_string(),
                    ),
                    ("Upload-Offset".to_string(), offset.to_string()),
                    (
                        "Content-Type".to_string(),
                        "application/offset+octet-stream".to_string(),
                    ),
                ],
                json_body: None,
                blob_body: Some(blob),
                with_credentials,
                abort_signal,
            }
        }

        fn abort_signal(mut self, abort_signal: Option<AbortSignal>) -> Self {
            self.abort_signal = abort_signal;
            self
        }
    }

    async fn fetch_json<T>(
        request: BrowserStorageRequest,
        operation: &'static str,
    ) -> StorageResult<T>
    where
        T: DeserializeOwned,
    {
        let response = send_request(request).await?;
        let envelope: StorageResponse<T> = match serde_json::from_str(&response.body) {
            Ok(envelope) => envelope,
            Err(_err) if !(200..300).contains(&response.status) => {
                return Err(StorageError::client(format!(
                    "{operation} failed with HTTP {}",
                    response.status
                )));
            }
            Err(err) => {
                return Err(StorageError::client(format!(
                    "parse {operation} response: {err}"
                )));
            }
        };
        let result = envelope.into_result();
        if !(200..300).contains(&response.status) && result.is_ok() {
            return Err(StorageError::client(format!(
                "{operation} failed with HTTP {}",
                response.status
            )));
        }
        result
    }

    /// Send a request that returns no body the client needs, surfacing the
    /// structured `StorageError` from the envelope on failure. Used by the
    /// multipart part path, where success is status-only and the server's
    /// session body is intentionally discarded.
    async fn fetch_no_content(
        request: BrowserStorageRequest,
        operation: &'static str,
    ) -> StorageResult<()> {
        let response = send_request(request).await?;
        if (200..300).contains(&response.status) {
            return Ok(());
        }
        // Non-2xx: prefer the structured error from the envelope (so the part
        // retry loop sees the real variant), else a generic client error.
        match serde_json::from_str::<StorageResponse<serde::de::IgnoredAny>>(&response.body) {
            Ok(envelope) => Err(envelope.into_result().err().unwrap_or_else(|| {
                StorageError::client(format!("{operation} failed with HTTP {}", response.status))
            })),
            Err(_) => Err(StorageError::client(format!(
                "{operation} failed with HTTP {}",
                response.status
            ))),
        }
    }

    fn ensure_tus_status(
        response: &BrowserStorageResponse,
        operation: &'static str,
        expected: &[u16],
    ) -> StorageResult<()> {
        if expected.contains(&response.status) {
            return Ok(());
        }
        // Surface "session is gone" via the structured variant so resume code
        // can distinguish a definitively-missing session from a transient
        // failure and avoid wiping a still-valid cached resume URL on a 5xx
        // blip.
        if response.status == 404 || response.status == 410 {
            return Err(StorageError::unknown_upload_session(operation.to_string()));
        }
        Err(StorageError::client(format!(
            "{operation} failed with HTTP {}{}",
            response.status,
            if response.body.is_empty() {
                String::new()
            } else {
                format!(": {}", response.body)
            }
        )))
    }

    fn parse_upload_offset_header(value: &str) -> StorageResult<u64> {
        value
            .parse::<u64>()
            .map_err(|_| StorageError::client(format!("invalid tus Upload-Offset: {value}")))
    }

    fn parse_upload_length_header(value: &str) -> StorageResult<u64> {
        value
            .parse::<u64>()
            .map_err(|_| StorageError::client(format!("invalid tus Upload-Length: {value}")))
    }

    fn encode_tus_metadata(metadata: &BTreeMap<String, String>) -> StorageResult<Option<String>> {
        if metadata.is_empty() {
            return Ok(None);
        }
        let mut encoded = Vec::with_capacity(metadata.len());
        for (key, value) in metadata {
            if key.is_empty()
                || key
                    .bytes()
                    .any(|byte| byte == b',' || byte.is_ascii_whitespace())
            {
                return Err(StorageError::client(format!(
                    "invalid tus metadata key: {key:?}"
                )));
            }
            if value.is_empty() {
                encoded.push(key.clone());
            } else {
                encoded.push(format!(
                    "{} {}",
                    key,
                    pocopine_codec::base64_encode(value.as_bytes())
                ));
            }
        }
        Ok(Some(encoded.join(",")))
    }

    async fn send_request(request: BrowserStorageRequest) -> StorageResult<BrowserStorageResponse> {
        #[cfg(any(test, feature = "test-utils"))]
        {
            let test_transport = TEST_TRANSPORT.with(|slot| slot.borrow().clone());
            if let Some(transport) = test_transport {
                return transport.request(request).await;
            }
        }
        real_fetch(request).await
    }

    async fn real_fetch(request: BrowserStorageRequest) -> StorageResult<BrowserStorageResponse> {
        let init = RequestInit::new();
        init.set_method(&request.method);
        if request.with_credentials {
            init.set_credentials(RequestCredentials::Include);
        }
        if let Some(signal) = request.abort_signal.as_ref() {
            init.set_signal(Some(signal));
        }
        if let Some(body) = request.json_body.as_ref() {
            init.set_body(&JsValue::from_str(body));
        }
        if let Some(blob) = request.blob_body.as_ref() {
            init.set_body(blob.as_ref());
        }

        let headers =
            Headers::new().map_err(|err| StorageError::client(format!("headers: {err:?}")))?;
        for (name, value) in &request.headers {
            headers
                .set(name, value)
                .map_err(|err| StorageError::client(format!("set header: {err:?}")))?;
        }
        init.set_headers(&headers);

        let req = Request::new_with_str_and_init(&request.url, &init)
            .map_err(|err| StorageError::client(format!("build request: {err:?}")))?;
        let window =
            web_sys::window().ok_or_else(|| StorageError::client("no window available"))?;
        let response_js = JsFuture::from(window.fetch_with_request(&req))
            .await
            .map_err(|err| StorageError::client(format!("fetch failed: {err:?}")))?;
        let response: Response = response_js
            .dyn_into()
            .map_err(|_| StorageError::client("fetch returned non-Response"))?;
        let status = response.status();
        let headers = collect_response_headers(&response)?;
        let text = response
            .text()
            .map_err(|err| StorageError::client(format!("read response: {err:?}")))?;
        let body_js = JsFuture::from(text)
            .await
            .map_err(|err| StorageError::client(format!("read response: {err:?}")))?;
        let body = body_js
            .as_string()
            .ok_or_else(|| StorageError::client("response body was not a string"))?;
        Ok(BrowserStorageResponse {
            status,
            headers,
            body,
        })
    }

    fn collect_response_headers(response: &Response) -> StorageResult<Vec<(String, String)>> {
        let headers = response.headers();
        let mut values = Vec::new();
        for name in [
            "location",
            "upload-offset",
            "upload-length",
            "upload-defer-length",
            "upload-expires",
            "upload-metadata",
            "tus-resumable",
            "tus-version",
            "tus-extension",
            "tus-max-size",
            "cache-control",
        ] {
            if let Some(value) = headers
                .get(name)
                .map_err(|err| StorageError::client(format!("read response header: {err:?}")))?
            {
                values.push((name.to_string(), value));
            }
        }
        Ok(values)
    }

    fn read_resume_session(key: &str) -> Option<UploadSession> {
        let value = local_storage()?.get_item(key).ok().flatten()?;
        serde_json::from_str(&value).ok()
    }

    fn write_resume_session(key: &str, session: &UploadSession) {
        if let (Some(storage), Ok(value)) = (local_storage(), serde_json::to_string(session)) {
            let _ = storage.set_item(key, &value);
        }
    }

    fn remove_resume_session(key: &str) {
        if let Some(storage) = local_storage() {
            let _ = storage.remove_item(key);
        }
    }

    fn read_upload_resume_url(key: &str) -> Option<String> {
        local_storage()?.get_item(key).ok().flatten()
    }

    fn write_upload_resume_url(key: &str, upload_url: &str) {
        if let Some(storage) = local_storage() {
            let _ = storage.set_item(key, upload_url);
        }
    }

    fn remove_upload_resume_url(key: &str) {
        if let Some(storage) = local_storage() {
            let _ = storage.remove_item(key);
        }
    }

    async fn retry_delay(base_ms: u32, attempt: u32) -> StorageResult<()> {
        let Some(delay) = base_ms.checked_mul(1_u32 << attempt.saturating_sub(1).min(5)) else {
            return delay_ms(u32::MAX as i32).await;
        };
        delay_ms(delay.min(i32::MAX as u32) as i32).await
    }

    async fn delay_ms(ms: i32) -> StorageResult<()> {
        if ms <= 0 {
            let _ = JsFuture::from(Promise::resolve(&JsValue::NULL)).await;
            return Ok(());
        }
        let promise = Promise::new(&mut |resolve, _reject| {
            let Some(window) = web_sys::window() else {
                let _ = resolve.call0(&JsValue::NULL);
                return;
            };
            let callback_resolve = resolve.clone();
            let callback = Closure::once(move || {
                let _ = callback_resolve.call0(&JsValue::NULL);
            });
            if window
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.as_ref().unchecked_ref(),
                    ms,
                )
                .is_ok()
            {
                callback.forget();
            } else {
                let _ = resolve.call0(&JsValue::NULL);
            }
        });
        let _ = JsFuture::from(promise)
            .await
            .map_err(|err| StorageError::client(format!("retry delay failed: {err:?}")))?;
        Ok(())
    }

    fn local_storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok().flatten()
    }

    fn uploads_url(endpoint: &str) -> String {
        endpoint.trim_end_matches('/').to_string() + "/uploads"
    }

    fn tus_uploads_url(endpoint: &str, scope: &str) -> String {
        format!(
            "{}/{}/uploads",
            endpoint.trim_end_matches('/'),
            pocopine_codec::percent_encode(scope)
        )
    }

    fn resolve_tus_location(create_url: &str, location: &str) -> StorageResult<String> {
        if location.starts_with('/') {
            // Origin-relative — implicitly same origin as `create_url`.
            return Ok(location.to_string());
        }
        if location.starts_with("http://") || location.starts_with("https://") {
            // Absolute URL: insist on same scheme + authority. A misconfigured
            // proxy or malicious server that returns
            // `Location: https://attacker.example.com/sess` here would
            // otherwise steer every subsequent PATCH (carrying file bytes)
            // and the cached resume URL to the foreign host.
            let location_origin = url_origin(location).ok_or_else(|| {
                StorageError::client(format!("malformed tus Location URL: {location}"))
            })?;
            let create_origin = url_origin(create_url).ok_or_else(|| {
                StorageError::client(format!("malformed tus create URL: {create_url}"))
            })?;
            if !location_origin.0.eq_ignore_ascii_case(create_origin.0)
                || !location_origin.1.eq_ignore_ascii_case(create_origin.1)
            {
                return Err(StorageError::client(format!(
                    "tus Location refers to a different origin: {location}"
                )));
            }
            return Ok(location.to_string());
        }
        // Path-relative against the create URL's directory.
        Ok(format!(
            "{}/{}",
            create_url.trim_end_matches('/'),
            location.trim_start_matches('/')
        ))
    }

    fn url_origin(url: &str) -> Option<(&str, &str)> {
        let (scheme, rest) = url.split_once("://")?;
        let authority = rest.split(['/', '?', '#']).next()?;
        if authority.is_empty() {
            return None;
        }
        Some((scheme, authority))
    }

    fn scope_url(endpoint: &str, scope: &str) -> String {
        format!(
            "{}/scopes/{}",
            endpoint.trim_end_matches('/'),
            pocopine_codec::percent_encode(scope)
        )
    }

    fn object_url(endpoint: &str, scope: &str, key: &str) -> String {
        format!(
            "{}/scopes/{}/objects/{}",
            endpoint.trim_end_matches('/'),
            pocopine_codec::percent_encode(scope),
            encode_object_key_path(key)
        )
    }

    fn object_read_url(endpoint: &str, scope: &str, key: &str) -> String {
        format!(
            "{}/scopes/{}/objects/read-url/{}",
            endpoint.trim_end_matches('/'),
            pocopine_codec::percent_encode(scope),
            encode_object_key_path(key)
        )
    }

    fn encode_object_key_path(key: &str) -> String {
        let mut encoded = String::new();
        for (index, segment) in key.split('/').enumerate() {
            if index > 0 {
                encoded.push('/');
            }
            encoded.push_str(&pocopine_codec::percent_encode(segment));
        }
        encoded
    }

    fn upload_url(endpoint: &str, session: &str) -> String {
        format!(
            "{}/uploads/{}",
            endpoint.trim_end_matches('/'),
            pocopine_codec::percent_encode(session)
        )
    }

    fn upload_complete_url(endpoint: &str, session: &str) -> String {
        format!("{}/complete", upload_url(endpoint, session))
    }

    fn empty_to_none(value: String) -> Option<String> {
        (!value.is_empty()).then_some(value)
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{
    BrowserStorageRequest, BrowserStorageResponse, BrowserStorageTransport, ResumableUploadBuilder,
    UploadBuilder,
};

#[cfg(all(target_arch = "wasm32", any(test, feature = "test-utils")))]
pub use wasm::{__reset_browser_transport_for_test, __set_browser_transport_for_test};
