use pocopine_core::{App, AppPlugin};

use crate::{
    StorageError, StorageResult, UploadPolicyDescriptor, UploadSession, UploadSessionId,
    STORAGE_ENDPOINT_PREFIX,
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
}

/// Scope-bound storage client.
#[derive(Clone, Debug)]
pub struct StorageScopeClient {
    endpoint: String,
    with_credentials: bool,
    scope: String,
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
#[derive(Debug)]
pub struct UploadBuilder;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;

    use js_sys::Promise;
    use serde::de::DeserializeOwned;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{
        AbortSignal, Blob, File, Headers, Request, RequestCredentials, RequestInit, Response,
    };

    use super::*;
    use crate::{
        CompleteUploadRequest, InitiateUploadRequest, ObjectRef, StorageResponse, UploadPhase,
        UploadProgress, UploadStrategy, STORAGE_PROTOCOL_V1,
    };

    const DEFAULT_CHUNK_SIZE: u64 = 1024 * 1024;

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
        pub body: String,
    }

    thread_local! {
        static TEST_TRANSPORT: RefCell<Option<Rc<dyn BrowserStorageTransport>>> =
            RefCell::new(None);
    }

    #[doc(hidden)]
    pub fn __set_browser_transport_for_test<T>(transport: T)
    where
        T: BrowserStorageTransport + 'static,
    {
        TEST_TRANSPORT.with(|slot| {
            *slot.borrow_mut() = Some(Rc::new(transport));
        });
    }

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

            let mut session = match self.resume_session.clone() {
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
            self.store_resume_session(&session);

            if session.strategy != UploadStrategy::Sequential {
                self.emit(UploadPhase::Failed, session.next_offset.unwrap_or(0));
                return Err(StorageError::unsupported(
                    "only sequential proxy uploads are implemented in pocopine-storage PR 1",
                ));
            }

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
                        Err(StorageError::OffsetMismatch { .. }) => {
                            session = self.inspect_session(&session).await?;
                            self.store_resume_session(&session);
                            offset = session.next_offset.unwrap_or(0);
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

            self.emit(UploadPhase::Completing, self.source.size);
            let object = self.complete(&session).await?;
            self.clear_resume_session();
            self.emit(UploadPhase::Complete, self.source.size);
            Ok(object)
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
                    upload_bytes_url(&self.endpoint, session.id.as_str()),
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
            if let Some(callback) = &self.progress {
                callback(UploadProgress {
                    bytes_sent,
                    bytes_total: Some(self.source.size),
                    current_part: None,
                    phase,
                });
            }
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
            Err(err) if !(200..300).contains(&response.status) => {
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

    async fn send_request(request: BrowserStorageRequest) -> StorageResult<BrowserStorageResponse> {
        let test_transport = TEST_TRANSPORT.with(|slot| slot.borrow().clone());
        if let Some(transport) = test_transport {
            return transport.request(request).await;
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
        let text = response
            .text()
            .map_err(|err| StorageError::client(format!("read response: {err:?}")))?;
        let body_js = JsFuture::from(Promise::from(text))
            .await
            .map_err(|err| StorageError::client(format!("read response: {err:?}")))?;
        let body = body_js
            .as_string()
            .ok_or_else(|| StorageError::client("response body was not a string"))?;
        Ok(BrowserStorageResponse { status, body })
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

    fn scope_url(endpoint: &str, scope: &str) -> String {
        format!(
            "{}/scopes/{}",
            endpoint.trim_end_matches('/'),
            percent_encode_path_segment(scope)
        )
    }

    fn upload_url(endpoint: &str, session: &str) -> String {
        format!(
            "{}/uploads/{}",
            endpoint.trim_end_matches('/'),
            percent_encode_path_segment(session)
        )
    }

    fn upload_bytes_url(endpoint: &str, session: &str) -> String {
        format!("{}/bytes", upload_url(endpoint, session))
    }

    fn upload_complete_url(endpoint: &str, session: &str) -> String {
        format!("{}/complete", upload_url(endpoint, session))
    }

    fn percent_encode_path_segment(value: &str) -> String {
        let mut encoded = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        encoded
    }

    fn empty_to_none(value: String) -> Option<String> {
        (!value.is_empty()).then_some(value)
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{
    BrowserStorageRequest, BrowserStorageResponse, BrowserStorageTransport, UploadBuilder,
    __reset_browser_transport_for_test, __set_browser_transport_for_test,
};
