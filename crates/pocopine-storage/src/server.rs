use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use pocopine_core::{ServerError, ServerResult};
use pocopine_server::auth::{Decision, Predicate, RequestContext};
use pocopine_server::axum::body::{to_bytes, Body};
use pocopine_server::axum::extract::{FromRequest, Path, State};
use pocopine_server::axum::http::{HeaderMap, Request};
use pocopine_server::axum::response::Json;
use pocopine_server::axum::routing::{get, patch, post};
use pocopine_server::{Server, ServerPlugin};
use uuid::Uuid;

use crate::{
    AnonymousUploadBinding, CompleteUpload, CompleteUploadRequest, InitiateUpload,
    InitiateUploadRequest, ObjectMetadata, PrincipalRef, SafeObjectKey, StorageBackendName,
    StorageError, StorageKey, StorageResult, UploadIntent, UploadPolicy, UploadPolicyDescriptor,
    UploadSession, UploadSessionId, UploadStrategy, STORAGE_ANON_COOKIE, STORAGE_PROTOCOL_V1,
    STORAGE_UPLOADS_PATH,
};

const MAX_PROXY_PATCH_BYTES: u64 = 64 * 1024 * 1024;

/// Future returned by storage backend methods.
pub type StorageBoxFuture<'a, T> = Pin<Box<dyn Future<Output = StorageResult<T>> + Send + 'a>>;

/// Future returned by a scope guard.
pub type StorageGuardFuture<'a> = Pin<Box<dyn Future<Output = ServerResult<()>> + Send + 'a>>;

/// Future returned by a storage key resolver.
pub type StorageKeyFuture<'a> =
    Pin<Box<dyn Future<Output = StorageResult<StorageKey>> + Send + 'a>>;

/// Actor bound to a storage request or upload session.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StorageActor {
    Principal(PrincipalRef),
    Anonymous(AnonymousUploadBinding),
    System(String),
}

/// Context passed into guards, resolvers, and backend adapters.
#[derive(Clone, Debug)]
pub struct StorageContext {
    pub actor: StorageActor,
    pub request: Option<RequestContext>,
}

impl StorageContext {
    pub fn from_request(request: RequestContext) -> Self {
        let actor = request
            .user
            .user()
            .map(|user| {
                StorageActor::Principal(PrincipalRef {
                    subject: user.id.clone(),
                    attributes: user
                        .claims
                        .iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|value| (key.clone(), value.to_string()))
                        })
                        .collect(),
                })
            })
            .unwrap_or_else(|| {
                StorageActor::Anonymous(AnonymousUploadBinding {
                    id: request
                        .session_id()
                        .or_else(|| request.cookie(STORAGE_ANON_COOKIE))
                        .unwrap_or_default()
                        .to_string(),
                })
            });
        Self {
            actor,
            request: Some(request),
        }
    }

    pub fn system(name: impl Into<String>) -> Self {
        Self {
            actor: StorageActor::System(name.into()),
            request: None,
        }
    }

    pub fn require_principal(&self) -> StorageResult<&PrincipalRef> {
        match &self.actor {
            StorageActor::Principal(principal) => Ok(principal),
            _ => Err(StorageError::unauthorized("login required")),
        }
    }
}

/// Server-side access check for a storage scope.
pub trait StorageScopeGuard: Send + Sync + 'static {
    fn check(&self, ctx: StorageContext) -> StorageGuardFuture<'_>;
}

impl<F, Fut> StorageScopeGuard for F
where
    F: Fn(StorageContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ServerResult<()>> + Send + 'static,
{
    fn check(&self, ctx: StorageContext) -> StorageGuardFuture<'_> {
        Box::pin((self)(ctx))
    }
}

struct PredicateScopeGuard<P>(P);

impl<P> StorageScopeGuard for PredicateScopeGuard<P>
where
    P: Predicate,
{
    fn check(&self, ctx: StorageContext) -> StorageGuardFuture<'_> {
        let result: ServerResult<()> = match &ctx.request {
            Some(request) => self.0.check(&request.user).into(),
            None => Decision::Allow.into(),
        };
        Box::pin(async move { result })
    }
}

/// App-owned resolver that maps an authorized upload intent to an object key.
pub trait StorageKeyResolver: Send + Sync + 'static {
    fn resolve_key<'a>(
        &'a self,
        ctx: &'a StorageContext,
        intent: &'a UploadIntent,
    ) -> StorageKeyFuture<'a>;
}

#[derive(Clone, Debug)]
struct GeneratedStorageKeyResolver;

impl StorageKeyResolver for GeneratedStorageKeyResolver {
    fn resolve_key<'a>(
        &'a self,
        _ctx: &'a StorageContext,
        intent: &'a UploadIntent,
    ) -> StorageKeyFuture<'a> {
        Box::pin(async move {
            let extension = intent
                .extension()
                .map(|extension| format!(".{extension}"))
                .unwrap_or_default();
            let key = SafeObjectKey::parse(format!(
                "{}/{}{}",
                intent.scope,
                intent.generated_object_id(),
                extension
            ))?;
            let mut metadata = ObjectMetadata::default();
            metadata.insert("original_name", intent.file_name());
            Ok(StorageKey::new(key).metadata(metadata))
        })
    }
}

/// Server-side backend contract for storage engines.
pub trait StorageBackend: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    fn initiate_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: InitiateUpload,
    ) -> StorageBoxFuture<'a, UploadSession>;

    fn inspect_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, UploadSession>;

    fn append_upload_bytes<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        offset: u64,
        bytes: Bytes,
    ) -> StorageBoxFuture<'a, UploadSession>;

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, crate::ObjectRef>;

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()>;
}

#[derive(Clone)]
struct RegisteredStorageScope {
    policy: UploadPolicy,
    write_guard: Option<Arc<dyn StorageScopeGuard>>,
    read_guard: Option<Arc<dyn StorageScopeGuard>>,
    delete_guard: Option<Arc<dyn StorageScopeGuard>>,
    key_resolver: Arc<dyn StorageKeyResolver>,
}

impl RegisteredStorageScope {
    async fn authorize_write(&self, ctx: StorageContext) -> ServerResult<()> {
        if let Some(guard) = &self.write_guard {
            guard.check(ctx).await
        } else {
            Ok(())
        }
    }

    async fn authorize_read(&self, ctx: StorageContext) -> ServerResult<()> {
        if let Some(guard) = &self.read_guard {
            guard.check(ctx).await
        } else {
            self.authorize_write(ctx).await
        }
    }

    async fn authorize_delete(&self, ctx: StorageContext) -> ServerResult<()> {
        if let Some(guard) = &self.delete_guard {
            guard.check(ctx).await
        } else {
            self.authorize_write(ctx).await
        }
    }
}

/// Server-registered storage scope.
#[derive(Clone)]
pub struct StorageScope {
    inner: RegisteredStorageScope,
}

impl StorageScope {
    pub fn builder(policy: UploadPolicy) -> StorageScopeBuilder {
        StorageScopeBuilder {
            policy,
            write_guard: None,
            read_guard: None,
            delete_guard: None,
            key_resolver: Arc::new(GeneratedStorageKeyResolver),
        }
    }
}

/// Builder for [`StorageScope`].
pub struct StorageScopeBuilder {
    policy: UploadPolicy,
    write_guard: Option<Arc<dyn StorageScopeGuard>>,
    read_guard: Option<Arc<dyn StorageScopeGuard>>,
    delete_guard: Option<Arc<dyn StorageScopeGuard>>,
    key_resolver: Arc<dyn StorageKeyResolver>,
}

impl StorageScopeBuilder {
    pub fn write_guard<G>(mut self, guard: G) -> Self
    where
        G: StorageScopeGuard,
    {
        self.write_guard = Some(Arc::new(guard));
        self
    }

    pub fn read_guard<G>(mut self, guard: G) -> Self
    where
        G: StorageScopeGuard,
    {
        self.read_guard = Some(Arc::new(guard));
        self
    }

    pub fn delete_guard<G>(mut self, guard: G) -> Self
    where
        G: StorageScopeGuard,
    {
        self.delete_guard = Some(Arc::new(guard));
        self
    }

    pub fn key_resolver<R>(mut self, resolver: R) -> Self
    where
        R: StorageKeyResolver,
    {
        self.key_resolver = Arc::new(resolver);
        self
    }

    pub fn build(self) -> StorageScope {
        StorageScope {
            inner: RegisteredStorageScope {
                policy: self.policy,
                write_guard: self.write_guard,
                read_guard: self.read_guard,
                delete_guard: self.delete_guard,
                key_resolver: self.key_resolver,
            },
        }
    }
}

#[derive(Clone)]
struct StorageServerInner {
    backends: Arc<HashMap<String, Arc<dyn StorageBackend>>>,
    scopes: Arc<HashMap<String, Arc<RegisteredStorageScope>>>,
}

/// Host-side storage server service.
#[derive(Clone)]
pub struct StorageServer {
    inner: Arc<StorageServerInner>,
}

impl StorageServer {
    pub fn builder() -> StorageServerBuilder {
        StorageServerBuilder::default()
    }

    pub async fn descriptor(
        &self,
        ctx: StorageContext,
        scope: &str,
    ) -> StorageResult<UploadPolicyDescriptor> {
        let scope_registration = self.scope(scope)?;
        scope_registration
            .authorize_read(ctx)
            .await
            .map_err(storage_auth_error)?;
        Ok(scope_registration.policy.descriptor(scope))
    }

    pub async fn initiate_upload(
        &self,
        ctx: StorageContext,
        request: InitiateUploadRequest,
    ) -> StorageResult<UploadSession> {
        if request.protocol != STORAGE_PROTOCOL_V1 {
            return Err(StorageError::invalid_value("protocol", request.protocol));
        }
        require_bound_actor(&ctx)?;
        let scope = self.scope(&request.scope)?;
        scope
            .authorize_write(ctx.clone())
            .await
            .map_err(storage_auth_error)?;
        scope.policy.validate_initiate(&request)?;

        if matches!(
            request.requested_strategy,
            UploadStrategy::SingleRequest | UploadStrategy::Multipart
        ) {
            return Err(StorageError::unsupported(
                "only sequential proxy uploads are implemented in pocopine-storage PR 1",
            ));
        }

        let intent = UploadIntent {
            scope: request.scope.clone(),
            file_name: request.file_name.clone(),
            size: request.size,
            content_type: request.content_type.clone(),
            metadata: request.metadata.clone(),
            requested_strategy: request.requested_strategy,
            generated_object_id: Uuid::new_v4().to_string(),
        };
        let storage_key = scope.key_resolver.resolve_key(&ctx, &intent).await?;
        let backend = self.backend(scope.policy.backend.as_str())?;
        backend
            .initiate_upload(
                &ctx,
                InitiateUpload {
                    scope: request.scope,
                    storage_key,
                    file_name: request.file_name,
                    size: request.size,
                    content_type: request.content_type,
                    metadata: request.metadata,
                    requested_strategy: request.requested_strategy,
                    policy: scope.policy.clone(),
                },
            )
            .await
    }

    pub async fn inspect_upload(
        &self,
        ctx: StorageContext,
        session: UploadSessionId,
    ) -> StorageResult<UploadSession> {
        require_bound_actor(&ctx)?;
        let (_backend, upload) = self.backend_for_session(&ctx, session).await?;
        let scope = self.scope(&upload.scope)?;
        scope
            .authorize_read(ctx)
            .await
            .map_err(storage_auth_error)?;
        Ok(upload)
    }

    pub async fn append_upload_bytes(
        &self,
        ctx: StorageContext,
        session: UploadSessionId,
        offset: u64,
        bytes: Bytes,
    ) -> StorageResult<UploadSession> {
        require_bound_actor(&ctx)?;
        let (backend, upload) = self.backend_for_session(&ctx, session.clone()).await?;
        let scope = self.scope(&upload.scope)?;
        scope
            .authorize_write(ctx.clone())
            .await
            .map_err(storage_auth_error)?;
        backend
            .append_upload_bytes(&ctx, session, offset, bytes)
            .await
    }

    pub async fn complete_upload(
        &self,
        ctx: StorageContext,
        request: CompleteUpload,
    ) -> StorageResult<crate::ObjectRef> {
        require_bound_actor(&ctx)?;
        let (backend, upload) = self
            .backend_for_session(&ctx, request.session.clone())
            .await?;
        let scope = self.scope(&upload.scope)?;
        scope
            .authorize_write(ctx.clone())
            .await
            .map_err(storage_auth_error)?;
        backend.complete_upload(&ctx, request).await
    }

    pub async fn abort_upload(
        &self,
        ctx: StorageContext,
        session: UploadSessionId,
    ) -> StorageResult<()> {
        require_bound_actor(&ctx)?;
        let Ok((backend, upload)) = self.backend_for_session(&ctx, session.clone()).await else {
            return Ok(());
        };
        let scope = self.scope(&upload.scope)?;
        scope
            .authorize_delete(ctx.clone())
            .await
            .map_err(storage_auth_error)?;
        backend.abort_upload(&ctx, session).await
    }

    fn scope(&self, scope: &str) -> StorageResult<Arc<RegisteredStorageScope>> {
        self.inner
            .scopes
            .get(scope)
            .cloned()
            .ok_or_else(|| StorageError::unknown_scope(scope))
    }

    fn backend(&self, backend: &str) -> StorageResult<Arc<dyn StorageBackend>> {
        self.inner
            .backends
            .get(backend)
            .cloned()
            .ok_or_else(|| StorageError::unknown_backend(backend))
    }

    async fn backend_for_session(
        &self,
        ctx: &StorageContext,
        session: UploadSessionId,
    ) -> StorageResult<(Arc<dyn StorageBackend>, UploadSession)> {
        for backend in self.inner.backends.values() {
            match backend.inspect_upload(ctx, session.clone()).await {
                Ok(upload) => return Ok((backend.clone(), upload)),
                Err(StorageError::UnknownUploadSession { .. }) => {}
                Err(StorageError::Forbidden { .. }) => {}
                Err(err) => return Err(err),
            }
        }
        Err(StorageError::unknown_upload_session(session.to_string()))
    }
}

/// Builder for [`StorageServer`].
#[derive(Default)]
pub struct StorageServerBuilder {
    backends: HashMap<String, Arc<dyn StorageBackend>>,
    scopes: HashMap<String, Arc<RegisteredStorageScope>>,
}

impl StorageServerBuilder {
    pub fn backend<B>(mut self, name: impl Into<String>, backend: B) -> StorageResult<Self>
    where
        B: StorageBackend,
    {
        let name = StorageBackendName::new(name.into())?;
        self.backends
            .insert(name.as_str().to_string(), Arc::new(backend));
        Ok(self)
    }

    pub fn public_scope(
        mut self,
        name: impl Into<String>,
        policy: UploadPolicy,
    ) -> StorageResult<Self> {
        self.insert_scope(name.into(), StorageScope::builder(policy).build())?;
        Ok(self)
    }

    pub fn guarded_scope<P>(
        mut self,
        name: impl Into<String>,
        policy: UploadPolicy,
        predicate: P,
    ) -> StorageResult<Self>
    where
        P: Predicate,
    {
        let guard = PredicateScopeGuard(predicate);
        let scope = StorageScope::builder(policy)
            .write_guard(guard)
            .read_guard(PredicateScopeGuard(pocopine_server::auth::require_auth()))
            .delete_guard(PredicateScopeGuard(pocopine_server::auth::require_auth()))
            .build();
        self.insert_scope(name.into(), scope)?;
        Ok(self)
    }

    pub fn guarded_scope_with<G>(
        mut self,
        name: impl Into<String>,
        policy: UploadPolicy,
        guard: G,
    ) -> StorageResult<Self>
    where
        G: StorageScopeGuard,
    {
        let scope = StorageScope::builder(policy).write_guard(guard).build();
        self.insert_scope(name.into(), scope)?;
        Ok(self)
    }

    pub fn scope(mut self, name: impl Into<String>, scope: StorageScope) -> StorageResult<Self> {
        self.insert_scope(name.into(), scope)?;
        Ok(self)
    }

    pub fn build(self) -> StorageServer {
        StorageServer {
            inner: Arc::new(StorageServerInner {
                backends: Arc::new(self.backends),
                scopes: Arc::new(self.scopes),
            }),
        }
    }

    fn insert_scope(&mut self, name: String, scope: StorageScope) -> StorageResult<()> {
        let name = StorageBackendName::new(name)?;
        self.scopes
            .insert(name.as_str().to_string(), Arc::new(scope.inner));
        Ok(())
    }
}

/// Server plugin that mounts storage routes and provides [`StorageServer`].
#[derive(Clone)]
pub struct StorageServerPlugin {
    storage: StorageServer,
}

/// Build a storage server plugin.
pub fn storage_server_plugin(storage: StorageServer) -> StorageServerPlugin {
    StorageServerPlugin { storage }
}

impl ServerPlugin for StorageServerPlugin {
    fn name(&self) -> &'static str {
        "pocopine-storage"
    }

    fn install(self, server: Server) -> Server {
        let storage = self.storage;
        server
            .provide_plugin(storage.clone())
            .route(
                "/__pocopine/storage/v1/scopes/:scope",
                get(scope_handler).with_state(storage.clone()),
            )
            .route(
                STORAGE_UPLOADS_PATH,
                post(initiate_handler).with_state(storage.clone()),
            )
            .route(
                "/__pocopine/storage/v1/uploads/:session",
                get(inspect_handler)
                    .delete(abort_handler)
                    .with_state(storage.clone()),
            )
            .route(
                "/__pocopine/storage/v1/uploads/:session/bytes",
                patch(bytes_handler).with_state(storage.clone()),
            )
            .route(
                "/__pocopine/storage/v1/uploads/:session/complete",
                post(complete_handler).with_state(storage),
            )
    }
}

async fn scope_handler(
    State(storage): State<StorageServer>,
    Path(scope): Path<String>,
    request: Request<Body>,
) -> Json<StorageResult<UploadPolicyDescriptor>> {
    Json(
        async {
            let ctx = context_from_request(request);
            storage.descriptor(ctx, &scope).await
        }
        .await,
    )
}

async fn initiate_handler(
    State(storage): State<StorageServer>,
    request: Request<Body>,
) -> Json<StorageResult<UploadSession>> {
    Json(
        async {
            let (ctx, request) = parse_json_request::<InitiateUploadRequest>(request).await?;
            storage.initiate_upload(ctx, request).await
        }
        .await,
    )
}

async fn inspect_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Json<StorageResult<UploadSession>> {
    Json(
        async {
            let ctx = context_from_request(request);
            let session = UploadSessionId::new(session)?;
            storage.inspect_upload(ctx, session).await
        }
        .await,
    )
}

async fn bytes_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Json<StorageResult<UploadSession>> {
    Json(
        async {
            let (parts, body) = request.into_parts();
            let ctx = StorageContext::from_request(RequestContext::from_parts(
                parts.method.clone(),
                parts.uri.clone(),
                parts.headers.clone(),
                parts.extensions.clone(),
            ));
            let offset = parts
                .headers
                .get("Upload-Offset")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| StorageError::invalid_value("Upload-Offset", "<missing>"))?
                .parse::<u64>()
                .map_err(|_| StorageError::invalid_value("Upload-Offset", "<invalid>"))?;
            let session = UploadSessionId::new(session)?;
            let upload = storage.inspect_upload(ctx.clone(), session.clone()).await?;
            storage
                .scope(&upload.scope)?
                .authorize_write(ctx.clone())
                .await
                .map_err(storage_auth_error)?;
            let max_body_bytes = max_patch_body_bytes(&upload, offset);
            reject_oversized_content_length(&parts.headers, max_body_bytes)?;
            let bytes = to_bytes(body, max_body_bytes as usize)
                .await
                .map_err(|err| {
                    StorageError::policy_rejected(format!("upload chunk is too large: {err}"))
                })?;
            storage
                .append_upload_bytes(ctx, session, offset, bytes)
                .await
        }
        .await,
    )
}

async fn complete_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Json<StorageResult<crate::ObjectRef>> {
    Json(
        async {
            let (ctx, request) =
                parse_optional_json_request::<CompleteUploadRequest>(request).await?;
            let session = UploadSessionId::new(session)?;
            storage
                .complete_upload(
                    ctx,
                    CompleteUpload {
                        session,
                        checksum: request.checksum,
                    },
                )
                .await
        }
        .await,
    )
}

async fn abort_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Json<StorageResult<()>> {
    Json(
        async {
            let ctx = context_from_request(request);
            let session = UploadSessionId::new(session)?;
            storage.abort_upload(ctx, session).await
        }
        .await,
    )
}

async fn parse_json_request<T>(request: Request<Body>) -> StorageResult<(StorageContext, T)>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let (parts, body) = request.into_parts();
    let ctx = StorageContext::from_request(RequestContext::from_parts(
        parts.method.clone(),
        parts.uri.clone(),
        parts.headers.clone(),
        parts.extensions.clone(),
    ));
    let request = Request::from_parts(parts, body);
    let Json(payload) = Json::<T>::from_request(request, &())
        .await
        .map_err(|err| StorageError::invalid_value("json", err.to_string()))?;
    Ok((ctx, payload))
}

async fn parse_optional_json_request<T>(
    request: Request<Body>,
) -> StorageResult<(StorageContext, T)>
where
    T: serde::de::DeserializeOwned + Default + Send + 'static,
{
    let (parts, body) = request.into_parts();
    let ctx = StorageContext::from_request(RequestContext::from_parts(
        parts.method.clone(),
        parts.uri.clone(),
        parts.headers.clone(),
        parts.extensions.clone(),
    ));
    let bytes = to_bytes(body, 1024 * 1024)
        .await
        .map_err(|err| StorageError::client(format!("read json body: {err}")))?;
    if bytes.is_empty() {
        return Ok((ctx, T::default()));
    }
    let payload = serde_json::from_slice(&bytes)
        .map_err(|err| StorageError::invalid_value("json", err.to_string()))?;
    Ok((ctx, payload))
}

fn context_from_request(request: Request<Body>) -> StorageContext {
    let (parts, _body) = request.into_parts();
    StorageContext::from_request(RequestContext::from_parts(
        parts.method,
        parts.uri,
        parts.headers,
        parts.extensions,
    ))
}

fn require_bound_actor(ctx: &StorageContext) -> StorageResult<()> {
    match &ctx.actor {
        StorageActor::Anonymous(binding) if binding.id.is_empty() => {
            Err(StorageError::unauthorized(
                "anonymous storage uploads require a session cookie or storage binding cookie",
            ))
        }
        _ => Ok(()),
    }
}

fn max_patch_body_bytes(upload: &UploadSession, offset: u64) -> u64 {
    let policy_cap = upload
        .plan
        .max_part_size
        .unwrap_or(MAX_PROXY_PATCH_BYTES)
        .min(MAX_PROXY_PATCH_BYTES);
    let size_cap = upload
        .size
        .map_or(u64::MAX, |size| size.saturating_sub(offset));
    policy_cap.min(size_cap).min(usize::MAX as u64)
}

fn reject_oversized_content_length(headers: &HeaderMap, limit: u64) -> StorageResult<()> {
    let Some(content_length) = headers.get("content-length") else {
        return Ok(());
    };
    let content_length = content_length
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| StorageError::invalid_value("Content-Length", "<invalid>"))?;
    if content_length > limit {
        return Err(StorageError::policy_rejected(format!(
            "upload chunk is too large: max {limit} bytes"
        )));
    }
    Ok(())
}

fn storage_auth_error(error: ServerError) -> StorageError {
    match error {
        ServerError::Unauthorized(message) => StorageError::unauthorized(message),
        ServerError::Forbidden(message) => StorageError::forbidden(message),
        ServerError::BadRequest(message) => StorageError::policy_rejected(message),
        ServerError::App(message) | ServerError::Network(message) => StorageError::backend(message),
    }
}
