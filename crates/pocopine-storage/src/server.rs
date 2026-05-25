use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use bytes::Bytes;
use pocopine_core::{ServerError, ServerResult};
use pocopine_server::auth::{Decision, Predicate, RequestContext};
use pocopine_server::axum::body::{to_bytes, Body};
use pocopine_server::axum::extract::{Path, State};
use pocopine_server::axum::http::{
    header::{CACHE_CONTROL, SET_COOKIE, VARY},
    HeaderMap, HeaderValue, Request, StatusCode,
};
use pocopine_server::axum::response::{IntoResponse, Json, Response};
use pocopine_server::axum::routing::{get, head, options, patch, post};
use pocopine_server::{Server, ServerPlugin};
use uuid::Uuid;

use crate::{
    AnonymousUploadBinding, CompleteUpload, CompleteUploadRequest, InitiateUpload,
    InitiateUploadRequest, ObjectMetadata, PrincipalRef, SafeObjectKey, StorageBackendName,
    StorageError, StorageKey, StorageResponse, StorageResult, UploadIntent, UploadPolicy,
    UploadPolicyDescriptor, UploadSession, UploadSessionId, UploadStrategy, STORAGE_ANON_COOKIE,
    STORAGE_PROTOCOL_V1, STORAGE_UPLOADS_PATH,
};

const MAX_PROXY_PATCH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSON_CONTROL_BYTES: usize = 64 * 1024;
const STORAGE_COOKIE_PATH: &str = "/__pocopine/storage";
const TUS_PROTOCOL_VERSION: &str = "1.0.0";
const TUS_UPLOADS_ROUTE: &str = "/__pocopine/storage/tus/v1/:scope/uploads";
const TUS_UPLOAD_ROUTE: &str = "/__pocopine/storage/tus/v1/:scope/uploads/:session";

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

impl StorageActor {
    /// Compare stable owner identity while ignoring mutable principal attributes.
    pub fn same_owner(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Principal(left), Self::Principal(right)) => left.subject == right.subject,
            (Self::Anonymous(left), Self::Anonymous(right)) => {
                !left.id.is_empty() && left.id == right.id
            }
            (Self::System(left), Self::System(right)) => left == right,
            _ => false,
        }
    }
}

/// Context passed into guards, resolvers, and backend adapters.
#[derive(Clone, Debug)]
pub struct StorageContext {
    pub actor: StorageActor,
    pub request: Option<RequestContext>,
}

impl StorageContext {
    pub fn from_request(request: RequestContext) -> Self {
        Self::from_request_with_anonymous_binding(request, None)
    }

    fn from_request_with_anonymous_binding(
        request: RequestContext,
        anonymous_binding: Option<String>,
    ) -> Self {
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
                        .or(anonymous_binding.as_deref())
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
            None => Decision::Deny("unauthorized").into(),
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
    trusted_origins: Arc<Vec<TrustedOrigin>>,
    secure_anonymous_cookies: bool,
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
        let (backend, upload) = match self.backend_for_session(&ctx, session.clone()).await {
            Ok(found) => found,
            Err(StorageError::UnknownUploadSession { .. }) => return Ok(()),
            Err(err) => return Err(err),
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
                Err(err) => return Err(err),
            }
        }
        Err(StorageError::unknown_upload_session(session.to_string()))
    }

    fn is_trusted_origin(&self, origin: &TrustedOrigin) -> bool {
        self.inner
            .trusted_origins
            .iter()
            .any(|trusted| trusted == origin)
    }

    fn secure_anonymous_cookies(&self) -> bool {
        self.inner.secure_anonymous_cookies
    }
}

/// Builder for [`StorageServer`].
pub struct StorageServerBuilder {
    backends: HashMap<String, Arc<dyn StorageBackend>>,
    scopes: HashMap<String, Arc<RegisteredStorageScope>>,
    trusted_origins: Vec<TrustedOrigin>,
    secure_anonymous_cookies: bool,
}

impl Default for StorageServerBuilder {
    fn default() -> Self {
        Self {
            backends: HashMap::new(),
            scopes: HashMap::new(),
            trusted_origins: Vec::new(),
            secure_anonymous_cookies: true,
        }
    }
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

    /// Trust an application origin for storage mutation requests.
    ///
    /// Origins are matched by scheme and authority. Configure this when the app
    /// is served behind a proxy whose external origin cannot be inferred from
    /// the request `Host` header.
    pub fn trusted_origin(mut self, origin: impl AsRef<str>) -> StorageResult<Self> {
        self.trusted_origins
            .push(TrustedOrigin::parse(origin.as_ref())?);
        Ok(self)
    }

    /// Add multiple trusted application origins.
    pub fn trusted_origins<I, S>(mut self, origins: I) -> StorageResult<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for origin in origins {
            self.trusted_origins
                .push(TrustedOrigin::parse(origin.as_ref())?);
        }
        Ok(self)
    }

    /// Whether the anonymous storage binding cookie should include `Secure`.
    ///
    /// Enabled by default for HTTPS deployments. Set this to `false` only for
    /// local HTTP development where browsers cannot persist `Secure` cookies.
    pub fn secure_anonymous_cookies(mut self, secure: bool) -> Self {
        self.secure_anonymous_cookies = secure;
        self
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
                trusted_origins: Arc::new(self.trusted_origins),
                secure_anonymous_cookies: self.secure_anonymous_cookies,
            }),
        }
    }

    fn insert_scope(&mut self, name: String, scope: StorageScope) -> StorageResult<()> {
        let name = StorageBackendName::new(name)?;
        scope.inner.policy.validate_configuration()?;
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

/// Server plugin that mounts a tus 1.0-compatible endpoint surface.
#[derive(Clone)]
pub struct StorageTusServerPlugin {
    storage: StorageServer,
}

/// Build a storage server plugin.
pub fn storage_server_plugin(storage: StorageServer) -> StorageServerPlugin {
    StorageServerPlugin { storage }
}

/// Build a tus 1.0-compatible storage server plugin.
///
/// This is intentionally a separate mount from Pocopine's native JSON storage
/// protocol. tus clients speak header/status-code based upload semantics, while
/// the native protocol keeps typed JSON envelopes for browser integrations.
pub fn storage_tus_server_plugin(storage: StorageServer) -> StorageTusServerPlugin {
    StorageTusServerPlugin { storage }
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

impl ServerPlugin for StorageTusServerPlugin {
    fn name(&self) -> &'static str {
        "pocopine-storage-tus"
    }

    fn install(self, server: Server) -> Server {
        let storage = self.storage;
        server
            .provide_plugin(storage.clone())
            .route(
                TUS_UPLOADS_ROUTE,
                options(tus_options_handler)
                    .post(tus_create_handler)
                    .with_state(storage.clone()),
            )
            .route(
                TUS_UPLOAD_ROUTE,
                head(tus_head_handler)
                    .patch(tus_patch_handler)
                    .delete(tus_delete_handler)
                    .with_state(storage),
            )
    }
}

async fn scope_handler(
    State(storage): State<StorageServer>,
    Path(scope): Path<String>,
    request: Request<Body>,
) -> Response {
    let request = context_from_request(request, &storage);
    storage_response(
        storage.descriptor(request.ctx, &scope).await,
        request.set_cookie,
    )
}

async fn initiate_handler(
    State(storage): State<StorageServer>,
    request: Request<Body>,
) -> Response {
    match parse_json_request::<InitiateUploadRequest>(request, &storage).await {
        Ok(request) => {
            let set_cookie = request.set_cookie;
            storage_response(
                storage.initiate_upload(request.ctx, request.payload).await,
                set_cookie,
            )
        }
        Err(err) => storage_response::<UploadSession>(Err(err), None),
    }
}

async fn inspect_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Response {
    let request = context_from_request(request, &storage);
    let response = storage_response(
        async {
            let session = UploadSessionId::new(session)?;
            storage.inspect_upload(request.ctx, session).await
        }
        .await,
        request.set_cookie,
    );
    no_store_response(response)
}

async fn bytes_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Response {
    match bytes_request(request, &storage).await {
        Ok(request) => {
            let set_cookie = request.set_cookie;
            storage_response(
                async {
                    let offset = request
                        .headers
                        .get("Upload-Offset")
                        .and_then(|value| value.to_str().ok())
                        .ok_or_else(|| StorageError::invalid_value("Upload-Offset", "<missing>"))?
                        .parse::<u64>()
                        .map_err(|_| StorageError::invalid_value("Upload-Offset", "<invalid>"))?;
                    let session = UploadSessionId::new(session)?;
                    let upload = storage
                        .inspect_upload(request.ctx.clone(), session.clone())
                        .await?;
                    let expected_offset = upload.next_offset.unwrap_or(0);
                    if expected_offset != offset {
                        return Err(StorageError::offset_mismatch(expected_offset, offset));
                    }
                    storage
                        .scope(&upload.scope)?
                        .authorize_write(request.ctx.clone())
                        .await
                        .map_err(storage_auth_error)?;
                    let max_body_bytes = max_patch_body_bytes(&upload, offset);
                    reject_oversized_content_length(&request.headers, max_body_bytes)?;
                    let bytes = to_bytes(request.body, max_body_bytes as usize)
                        .await
                        .map_err(|_err| StorageError::payload_too_large(max_body_bytes))?;
                    storage
                        .append_upload_bytes(request.ctx, session, offset, bytes)
                        .await
                }
                .await,
                set_cookie,
            )
        }
        Err(err) => storage_response::<UploadSession>(Err(err), None),
    }
}

async fn complete_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Response {
    match parse_optional_json_request::<CompleteUploadRequest>(request, &storage).await {
        Ok(request) => {
            let set_cookie = request.set_cookie;
            storage_response(
                async {
                    let payload = request.payload;
                    let session = UploadSessionId::new(session)?;
                    storage
                        .complete_upload(
                            request.ctx,
                            CompleteUpload {
                                session,
                                checksum: payload.checksum,
                            },
                        )
                        .await
                }
                .await,
                set_cookie,
            )
        }
        Err(err) => storage_response::<crate::ObjectRef>(Err(err), None),
    }
}

async fn abort_handler(
    State(storage): State<StorageServer>,
    Path(session): Path<String>,
    request: Request<Body>,
) -> Response {
    match mutation_context_from_request(request, &storage) {
        Ok(request) => {
            let set_cookie = request.set_cookie;
            storage_response(
                async {
                    let session = UploadSessionId::new(session)?;
                    storage.abort_upload(request.ctx, session).await
                }
                .await,
                set_cookie,
            )
        }
        Err(err) => storage_response::<()>(Err(err), None),
    }
}

async fn tus_options_handler(
    State(storage): State<StorageServer>,
    Path(scope): Path<String>,
    request: Request<Body>,
) -> Response {
    tus_route_response(
        async {
            let _ = request;
            let scope = storage.scope(&scope)?;
            let mut response = tus_empty_response(StatusCode::NO_CONTENT);
            set_response_header(&mut response, "tus-version", TUS_PROTOCOL_VERSION)?;
            set_response_header(
                &mut response,
                "tus-extension",
                "creation,creation-with-upload,creation-defer-length,termination,expiration",
            )?;
            set_response_header(
                &mut response,
                "tus-max-size",
                scope.policy.max_bytes.to_string(),
            )?;
            Ok(response)
        }
        .await,
    )
}

async fn tus_create_handler(
    State(storage): State<StorageServer>,
    Path(scope): Path<String>,
    request: Request<Body>,
) -> Response {
    tus_route_response(
        async {
            let request = bytes_request(request, &storage).await?;
            require_tus_version(&request.headers)?;
            let metadata = tus_metadata(&request.headers)?;
            let size = tus_upload_length(&request.headers)?;
            let mut upload = storage
                .initiate_upload(
                    request.ctx.clone(),
                    InitiateUploadRequest {
                        protocol: STORAGE_PROTOCOL_V1.to_string(),
                        scope: scope.clone(),
                        file_name: metadata
                            .file_name
                            .clone()
                            .unwrap_or_else(|| "upload".to_string()),
                        size,
                        content_type: metadata.content_type.clone(),
                        metadata: metadata.values,
                        requested_strategy: UploadStrategy::Sequential,
                    },
                )
                .await?;

            let mut offset = upload.next_offset.unwrap_or(0);
            if content_length(&request.headers).unwrap_or(0) > 0
                || has_tus_octet_stream(&request.headers)
            {
                require_tus_octet_stream(&request.headers)?;
                let max_body_bytes = max_patch_body_bytes(&upload, 0);
                let bytes = to_bytes(request.body, max_body_bytes as usize)
                    .await
                    .map_err(|_err| StorageError::payload_too_large(max_body_bytes))?;
                if !bytes.is_empty() {
                    upload = storage
                        .append_upload_bytes(request.ctx.clone(), upload.id.clone(), 0, bytes)
                        .await?;
                    offset = upload.next_offset.unwrap_or(offset);
                }
            }
            maybe_tus_complete(&storage, request.ctx.clone(), &upload).await?;

            let mut response = tus_empty_response(StatusCode::CREATED);
            set_response_header(
                &mut response,
                "location",
                format!(
                    "/__pocopine/storage/tus/v1/{}/uploads/{}",
                    scope,
                    upload.id.as_str()
                ),
            )?;
            set_response_header(&mut response, "upload-offset", offset.to_string())?;
            set_tus_upload_length_headers(&mut response, size)?;
            set_response_header(
                &mut response,
                "upload-expires",
                upload
                    .expires_at
                    .format(&time::format_description::well_known::Rfc2822)
                    .map_err(|err| {
                        StorageError::backend(format!("format tus expiration: {err}"))
                    })?,
            )?;
            apply_set_cookie(&mut response, request.set_cookie);
            Ok(response)
        }
        .await,
    )
}

async fn tus_head_handler(
    State(storage): State<StorageServer>,
    Path((_scope, session)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    tus_route_response(
        async {
            let request = context_from_request(request, &storage);
            require_tus_version(request.ctx.request.as_ref().unwrap().headers())?;
            let session = UploadSessionId::new(session)?;
            let upload = storage.inspect_upload(request.ctx, session).await?;
            let mut response = tus_empty_response(StatusCode::NO_CONTENT);
            set_response_header(
                &mut response,
                "upload-offset",
                upload.next_offset.unwrap_or(0).to_string(),
            )?;
            set_tus_upload_length_headers(&mut response, upload.size)?;
            set_response_header(
                &mut response,
                "upload-expires",
                upload
                    .expires_at
                    .format(&time::format_description::well_known::Rfc2822)
                    .map_err(|err| {
                        StorageError::backend(format!("format tus expiration: {err}"))
                    })?,
            )?;
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            apply_set_cookie(&mut response, request.set_cookie);
            Ok(response)
        }
        .await,
    )
}

async fn tus_patch_handler(
    State(storage): State<StorageServer>,
    Path((_scope, session)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    tus_route_response(
        async {
            let request = bytes_request(request, &storage).await?;
            require_tus_version(&request.headers)?;
            require_tus_octet_stream(&request.headers)?;
            let offset = upload_offset(&request.headers)?;
            let session = UploadSessionId::new(session)?;
            let upload = storage
                .inspect_upload(request.ctx.clone(), session.clone())
                .await?;
            let expected_offset = upload.next_offset.unwrap_or(0);
            if expected_offset != offset {
                return Err(StorageError::offset_mismatch(expected_offset, offset).into());
            }
            storage
                .scope(&upload.scope)?
                .authorize_write(request.ctx.clone())
                .await
                .map_err(storage_auth_error)?;
            let max_body_bytes = max_patch_body_bytes(&upload, offset);
            reject_oversized_content_length(&request.headers, max_body_bytes)?;
            let bytes = to_bytes(request.body, max_body_bytes as usize)
                .await
                .map_err(|_err| StorageError::payload_too_large(max_body_bytes))?;
            let updated = storage
                .append_upload_bytes(request.ctx.clone(), session, offset, bytes)
                .await?;
            maybe_tus_complete(&storage, request.ctx, &updated).await?;

            let mut response = tus_empty_response(StatusCode::NO_CONTENT);
            set_response_header(
                &mut response,
                "upload-offset",
                updated.next_offset.unwrap_or(offset).to_string(),
            )?;
            apply_set_cookie(&mut response, request.set_cookie);
            Ok(response)
        }
        .await,
    )
}

async fn tus_delete_handler(
    State(storage): State<StorageServer>,
    Path((_scope, session)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    tus_route_response(
        async {
            let request = mutation_context_from_request(request, &storage)?;
            require_tus_version(request.ctx.request.as_ref().unwrap().headers())?;
            let session = UploadSessionId::new(session)?;
            storage.abort_upload(request.ctx, session).await?;
            let mut response = tus_empty_response(StatusCode::NO_CONTENT);
            apply_set_cookie(&mut response, request.set_cookie);
            Ok(response)
        }
        .await,
    )
}

struct StorageRequest {
    ctx: StorageContext,
    set_cookie: Option<String>,
}

struct ParsedStorageRequest<T> {
    ctx: StorageContext,
    payload: T,
    set_cookie: Option<String>,
}

struct BytesStorageRequest {
    ctx: StorageContext,
    headers: HeaderMap,
    body: Body,
    set_cookie: Option<String>,
}

async fn parse_json_request<T>(
    request: Request<Body>,
    storage: &StorageServer,
) -> StorageResult<ParsedStorageRequest<T>>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let (parts, body) = request.into_parts();
    reject_cross_site_mutation(storage, &parts.headers)?;
    let request = context_from_parts(
        parts.method.clone(),
        parts.uri.clone(),
        parts.headers.clone(),
        parts.extensions.clone(),
        storage,
    );
    require_json_content_type(&parts.headers)?;
    let bytes = to_bytes(body, MAX_JSON_CONTROL_BYTES)
        .await
        .map_err(|err| StorageError::client(format!("read json body: {err}")))?;
    if bytes.is_empty() {
        return Err(StorageError::invalid_value("json", "<empty>"));
    }
    let payload = serde_json::from_slice(&bytes)
        .map_err(|err| StorageError::invalid_value("json", err.to_string()))?;
    Ok(ParsedStorageRequest {
        ctx: request.ctx,
        payload,
        set_cookie: request.set_cookie,
    })
}

async fn parse_optional_json_request<T>(
    request: Request<Body>,
    storage: &StorageServer,
) -> StorageResult<ParsedStorageRequest<T>>
where
    T: serde::de::DeserializeOwned + Default + Send + 'static,
{
    let (parts, body) = request.into_parts();
    reject_cross_site_mutation(storage, &parts.headers)?;
    let request = context_from_parts(
        parts.method.clone(),
        parts.uri.clone(),
        parts.headers.clone(),
        parts.extensions.clone(),
        storage,
    );
    let bytes = to_bytes(body, MAX_JSON_CONTROL_BYTES)
        .await
        .map_err(|err| StorageError::client(format!("read json body: {err}")))?;
    if bytes.is_empty() {
        return Ok(ParsedStorageRequest {
            ctx: request.ctx,
            payload: T::default(),
            set_cookie: request.set_cookie,
        });
    }
    require_json_content_type(&parts.headers)?;
    let payload = serde_json::from_slice(&bytes)
        .map_err(|err| StorageError::invalid_value("json", err.to_string()))?;
    Ok(ParsedStorageRequest {
        ctx: request.ctx,
        payload,
        set_cookie: request.set_cookie,
    })
}

async fn bytes_request(
    request: Request<Body>,
    storage: &StorageServer,
) -> StorageResult<BytesStorageRequest> {
    let (parts, body) = request.into_parts();
    reject_cross_site_mutation(storage, &parts.headers)?;
    let request = context_from_parts(
        parts.method,
        parts.uri,
        parts.headers.clone(),
        parts.extensions,
        storage,
    );
    Ok(BytesStorageRequest {
        ctx: request.ctx,
        headers: parts.headers,
        body,
        set_cookie: request.set_cookie,
    })
}

fn mutation_context_from_request(
    request: Request<Body>,
    storage: &StorageServer,
) -> StorageResult<StorageRequest> {
    let (parts, _body) = request.into_parts();
    reject_cross_site_mutation(storage, &parts.headers)?;
    Ok(context_from_parts(
        parts.method,
        parts.uri,
        parts.headers,
        parts.extensions,
        storage,
    ))
}

fn context_from_request(request: Request<Body>, storage: &StorageServer) -> StorageRequest {
    let (parts, _body) = request.into_parts();
    context_from_parts(
        parts.method,
        parts.uri,
        parts.headers,
        parts.extensions,
        storage,
    )
}

fn context_from_parts(
    method: pocopine_server::axum::http::Method,
    uri: pocopine_server::axum::http::Uri,
    headers: HeaderMap,
    extensions: pocopine_server::axum::http::Extensions,
    storage: &StorageServer,
) -> StorageRequest {
    let request = RequestContext::from_parts(method, uri, headers, extensions);
    let anonymous_binding = if request.user.user().is_none()
        && request.session_id().is_none()
        && request.cookie(STORAGE_ANON_COOKIE).is_none()
    {
        Some(Uuid::new_v4().to_string())
    } else {
        None
    };
    let set_cookie = anonymous_binding
        .as_ref()
        .map(|binding| anonymous_binding_cookie(binding, storage.secure_anonymous_cookies()));
    StorageRequest {
        ctx: StorageContext::from_request_with_anonymous_binding(request, anonymous_binding),
        set_cookie,
    }
}

fn anonymous_binding_cookie(binding: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{STORAGE_ANON_COOKIE}={binding}; Path={STORAGE_COOKIE_PATH}; HttpOnly; SameSite=Strict{secure}"
    )
}

#[derive(Debug)]
enum TusError {
    VersionMismatch,
    Storage(StorageError),
}

impl From<StorageError> for TusError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

struct TusMetadata {
    file_name: Option<String>,
    content_type: Option<String>,
    values: BTreeMap<String, String>,
}

async fn maybe_tus_complete(
    storage: &StorageServer,
    ctx: StorageContext,
    upload: &UploadSession,
) -> Result<(), TusError> {
    if upload
        .size
        .is_some_and(|size| upload.next_offset == Some(size))
    {
        storage
            .complete_upload(
                ctx,
                CompleteUpload {
                    session: upload.id.clone(),
                    checksum: None,
                },
            )
            .await?;
    }
    Ok(())
}

fn require_tus_version(headers: &HeaderMap) -> Result<(), TusError> {
    match headers
        .get("tus-resumable")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) if value == TUS_PROTOCOL_VERSION => Ok(()),
        _ => Err(TusError::VersionMismatch),
    }
}

fn tus_upload_length(headers: &HeaderMap) -> StorageResult<Option<u64>> {
    let upload_length = headers
        .get("upload-length")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| StorageError::invalid_value("Upload-Length", value.to_string()))
        })
        .transpose()?;
    let defer_length = headers
        .get("upload-defer-length")
        .and_then(|value| value.to_str().ok());
    if upload_length.is_some() && defer_length.is_some() {
        return Err(StorageError::invalid_value(
            "Upload-Length",
            "cannot be combined with Upload-Defer-Length",
        ));
    }
    match (upload_length, defer_length) {
        (Some(length), None) => Ok(Some(length)),
        (None, Some("1")) => Ok(None),
        (None, Some(value)) => Err(StorageError::invalid_value(
            "Upload-Defer-Length",
            value.to_string(),
        )),
        (None, None) => Err(StorageError::invalid_value("Upload-Length", "<missing>")),
        (Some(_), Some(_)) => unreachable!(),
    }
}

fn upload_offset(headers: &HeaderMap) -> StorageResult<u64> {
    headers
        .get("upload-offset")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| StorageError::invalid_value("Upload-Offset", "<missing>"))?
        .parse::<u64>()
        .map_err(|_| StorageError::invalid_value("Upload-Offset", "<invalid>"))
}

fn tus_metadata(headers: &HeaderMap) -> StorageResult<TusMetadata> {
    let mut values = BTreeMap::new();
    if let Some(header) = headers
        .get("upload-metadata")
        .and_then(|value| value.to_str().ok())
    {
        for pair in header.split(',') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let (key, encoded) = pair
                .split_once(' ')
                .ok_or_else(|| StorageError::invalid_value("Upload-Metadata", pair.to_string()))?;
            if key.is_empty() {
                return Err(StorageError::invalid_value("Upload-Metadata", pair));
            }
            let bytes = BASE64_STANDARD
                .decode(encoded)
                .map_err(|_| StorageError::invalid_value("Upload-Metadata", pair.to_string()))?;
            let value = String::from_utf8(bytes)
                .map_err(|_| StorageError::invalid_value("Upload-Metadata", pair.to_string()))?;
            values.insert(key.to_string(), value);
        }
    }
    Ok(TusMetadata {
        file_name: values.get("filename").cloned(),
        content_type: values.get("filetype").cloned(),
        values,
    })
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn require_tus_octet_stream(headers: &HeaderMap) -> StorageResult<()> {
    if has_tus_octet_stream(headers) {
        Ok(())
    } else {
        let content_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        Err(StorageError::invalid_value(
            "Content-Type",
            content_type.to_string(),
        ))
    }
}

fn has_tus_octet_stream(headers: &HeaderMap) -> bool {
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    content_type.split(';').next().is_some_and(|value| {
        value
            .trim()
            .eq_ignore_ascii_case("application/offset+octet-stream")
    })
}

fn set_tus_upload_length_headers(response: &mut Response, size: Option<u64>) -> StorageResult<()> {
    match size {
        Some(size) => set_response_header(response, "upload-length", size.to_string()),
        None => set_response_header(response, "upload-defer-length", "1"),
    }
}

fn tus_route_response(result: Result<Response, TusError>) -> Response {
    match result {
        Ok(response) => response,
        Err(error) => tus_error_response(error),
    }
}

fn tus_empty_response(status: StatusCode) -> Response {
    let mut response = Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("valid tus response");
    response.headers_mut().insert(
        "tus-resumable",
        HeaderValue::from_static(TUS_PROTOCOL_VERSION),
    );
    response
}

fn tus_error_response(error: TusError) -> Response {
    let version_mismatch = matches!(error, TusError::VersionMismatch);
    let (status, message) = match error {
        TusError::VersionMismatch => (
            StatusCode::PRECONDITION_FAILED,
            "tus version mismatch".to_string(),
        ),
        TusError::Storage(error) => (tus_storage_error_status(&error), error.to_string()),
    };
    let mut response = Response::builder()
        .status(status)
        .body(Body::from(message))
        .expect("valid tus error response");
    response.headers_mut().insert(
        "tus-resumable",
        HeaderValue::from_static(TUS_PROTOCOL_VERSION),
    );
    if version_mismatch {
        response.headers_mut().insert(
            "tus-version",
            HeaderValue::from_static(TUS_PROTOCOL_VERSION),
        );
    }
    response
}

fn tus_storage_error_status(error: &StorageError) -> StatusCode {
    match error {
        StorageError::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        StorageError::InvalidValue { field, .. } if field == "Content-Type" => {
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        }
        StorageError::InvalidValue { field, .. } if field == "Tus-Resumable" => {
            StatusCode::PRECONDITION_FAILED
        }
        StorageError::UploadComplete { .. } => {
            StatusCode::from_u16(423).expect("423 Locked is a valid HTTP status")
        }
        _ => storage_error_status(error),
    }
}

fn set_response_header(
    response: &mut Response,
    name: &'static str,
    value: impl AsRef<str>,
) -> StorageResult<()> {
    let value = HeaderValue::from_str(value.as_ref())
        .map_err(|_| StorageError::invalid_value(name, value.as_ref()))?;
    response.headers_mut().insert(name, value);
    Ok(())
}

fn apply_set_cookie(response: &mut Response, set_cookie: Option<String>) {
    if let Some(set_cookie) = set_cookie {
        if let Ok(value) = HeaderValue::from_str(&set_cookie) {
            response.headers_mut().insert(SET_COOKIE, value);
        }
    }
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
        return Err(StorageError::payload_too_large(limit));
    }
    Ok(())
}

fn reject_cross_site_mutation(storage: &StorageServer, headers: &HeaderMap) -> StorageResult<()> {
    if let Some(fetch_site) = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
    {
        if fetch_site.eq_ignore_ascii_case("same-origin")
            || fetch_site.eq_ignore_ascii_case("same-site")
        {
            return Ok(());
        }
        if fetch_site.eq_ignore_ascii_case("cross-site") || fetch_site.eq_ignore_ascii_case("none")
        {
            return Err(StorageError::forbidden(
                "cross-site storage mutation rejected",
            ));
        }
    }

    let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) else {
        return Err(StorageError::forbidden(
            "storage mutation origin could not be validated",
        ));
    };
    let origin = TrustedOrigin::parse(origin)?;
    if storage.is_trusted_origin(&origin) || origin_matches_request_host(&origin, headers) {
        Ok(())
    } else {
        return Err(StorageError::forbidden(
            "cross-origin storage mutation rejected",
        ));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrustedOrigin {
    scheme: String,
    authority: String,
}

impl TrustedOrigin {
    fn parse(origin: &str) -> StorageResult<Self> {
        let (scheme, rest) = origin
            .trim()
            .split_once("://")
            .ok_or_else(|| StorageError::invalid_value("Origin", origin.to_string()))?;
        let authority = rest
            .split('/')
            .next()
            .filter(|authority| !authority.is_empty())
            .ok_or_else(|| StorageError::invalid_value("Origin", origin.to_string()))?;
        if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
            return Err(StorageError::invalid_value("Origin", origin.to_string()));
        }
        Ok(Self {
            scheme: scheme.to_ascii_lowercase(),
            authority: authority.to_ascii_lowercase(),
        })
    }
}

fn origin_matches_request_host(origin: &TrustedOrigin, headers: &HeaderMap) -> bool {
    let Some(host) = headers.get("host").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if !origin.authority.eq_ignore_ascii_case(host) {
        return false;
    }
    origin.scheme == "https" || (origin.scheme == "http" && is_localhost_authority(host))
}

fn is_localhost_authority(authority: &str) -> bool {
    let host = authority
        .rsplit_once(':')
        .map(|(host, _port)| host)
        .unwrap_or(authority);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

fn require_json_content_type(headers: &HeaderMap) -> StorageResult<()> {
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        Ok(())
    } else {
        Err(StorageError::invalid_value(
            "Content-Type",
            content_type.to_string(),
        ))
    }
}

fn storage_response<T>(result: StorageResult<T>, set_cookie: Option<String>) -> Response
where
    T: serde::Serialize,
{
    let status = result
        .as_ref()
        .err()
        .map(storage_error_status)
        .unwrap_or(StatusCode::OK);
    let mut response = (status, Json(StorageResponse::from_result(result))).into_response();
    if let Some(set_cookie) = set_cookie {
        if let Ok(value) = HeaderValue::from_str(&set_cookie) {
            response.headers_mut().insert(SET_COOKIE, value);
        }
    }
    response
}

fn no_store_response(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(VARY, HeaderValue::from_static("Cookie"));
    response
}

fn storage_error_status(error: &StorageError) -> StatusCode {
    match error {
        StorageError::InvalidValue { .. } => StatusCode::BAD_REQUEST,
        StorageError::UnknownScope { .. }
        | StorageError::UnknownBackend { .. }
        | StorageError::UnknownUploadSession { .. } => StatusCode::NOT_FOUND,
        StorageError::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
        StorageError::Forbidden { .. } => StatusCode::FORBIDDEN,
        StorageError::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        StorageError::PolicyRejected { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        StorageError::Unsupported { .. } => StatusCode::NOT_IMPLEMENTED,
        StorageError::OffsetMismatch { .. } => StatusCode::CONFLICT,
        StorageError::UploadComplete { .. } | StorageError::UploadClosed { .. } => {
            StatusCode::CONFLICT
        }
        StorageError::Client { .. } => StatusCode::BAD_REQUEST,
        StorageError::Backend { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn storage_auth_error(error: ServerError) -> StorageError {
    match error {
        ServerError::Unauthorized(message) => StorageError::unauthorized(message),
        ServerError::Forbidden(message) => StorageError::forbidden(message),
        ServerError::BadRequest(message) => StorageError::policy_rejected(message),
        ServerError::App(message) | ServerError::Network(message) => StorageError::backend(message),
    }
}
