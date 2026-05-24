# RFC 082 - Storage-agnostic file/object storage

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-05-24 |
| **Related** | [RFC 002 - Application framework, stores, server functions](./rfc-002-app-stores-servers.md), [RFC 023 - Pine MVP](./rfc-023-pine-mvp.md), [RFC 066 - Server-function auth and access policy](./rfc-066-server-function-auth.md), [RFC 076 - App plugin lifecycle](./rfc-076-app-plugin-lifecycle.md), [RFC 077 - Server plugin lifecycle](./rfc-077-server-plugin-lifecycle.md), [RFC 080 - Heroku-style deploy contract](./rfc-080-deploy-contract.md) |
| **Supersedes** | - |

## 1. Summary

Add an explicit `pocopine-storage` extension crate for user-uploaded
files and application object storage. The crate defines a storage-agnostic
upload protocol, backend trait, `ServerPlugin`, `AppPlugin`, browser
upload client, and shared metadata contract that Pine upload primitives
can consume.

The goal is not to make Pocopine a storage provider. The goal is to make
common file workflows first-class while keeping provider choice behind a
public backend contract:

- local filesystem or memory backends for tests and development,
- S3-compatible backends such as AWS S3, Cloudflare R2, MinIO, or Supabase
  Storage,
- user-authored or third-party backends for teams with existing storage
  engines,
- later GCS, Azure Blob, and host-specific backends without changing Pine
  components or application code.

The hard part is resumable transfer. `pocopine-storage` must own a stable
session protocol so the UI can create, resume, retry, complete, abort, and
observe uploads without knowing whether the backend stores bytes through
offset appends, multipart parts, block lists, signed provider URLs, or a
framework proxy route.

```rust
#[cfg(pocopine_host)]
pub fn storage_server() -> pocopine_storage::StorageServer {
    pocopine_storage::StorageServer::builder()
        .backend("uploads", s3_backend_from_env())
        .scope(
            "avatars",
            pocopine_storage::StorageScope::builder(avatar_policy())
                .write_guard(pocopine_auth::require_auth())
                .read_guard(pocopine_auth::require_auth())
                .delete_guard(pocopine_auth::require_auth())
                .key_resolver(AvatarStorageKeys)
                .build(),
        )
        .build()
}
```

```rust
#[cfg(target_arch = "wasm32")]
async fn upload_avatar(file: web_sys::File) -> Result<ObjectRef, StorageError> {
    pocopine_storage::StorageClient::new()
        .scope("avatars")
        .upload(file)
        .strategy(UploadStrategy::Auto)
        .send()
        .await
}
```

Pine then builds on the same client:

```html
<pine-dropzone-root scope="avatars" pp-model:items="avatar_uploads" />
```

The backend owns storage mechanics, resumable-session state, part receipts,
and signed upload targets. The scope owns authorization, upload policy, and
key resolution through an app-provided `StorageKeyResolver`. The frontend
owns file selection, progress, cancellation, retry, resume, and ergonomic
component state. The shared contract lets the UI mirror backend policy
without making client-side validation authoritative.

## 2. Motivation

File upload is a cross-cutting application feature. Product teams need
avatars, attachments, imports, exports, generated files, rich-text embeds,
and private downloads. Without a framework-level contract each app ends up
assembling the same pieces:

1. A browser file input or drop zone.
2. Client-side size and content-type checks.
3. A server endpoint to authorize the upload.
4. Provider-specific signing or proxy upload code.
5. Completion metadata that the application stores in its database.
6. Download or preview URLs with the right privacy model.
7. Cleanup for abandoned uploads.

Those pieces must agree on the same policy. Duplicating the policy between
a Pine dropzone and a server route creates drift: the UI says PNG is
allowed but the backend rejects it, the frontend accepts a 20 MiB video
while the storage scope only allows 5 MiB, or the browser uploads directly
to a bucket key the server would never have chosen.

The harder drift is protocol drift. For large files the app needs
resumption after a tab crash, per-chunk retry, cancellation, cleanup of
abandoned provider resources, and finalization metadata that proves which
bytes became the object. Different providers solve this differently:

- tus and the current HTTP resumable-upload draft are offset-oriented:
  create an upload resource, inspect the offset, append bytes, and mark
  completion.
- S3-compatible storage is part-oriented: create a multipart upload,
  upload numbered parts, keep each part's receipt, and complete with the
  ordered receipt list.
- Google Cloud Storage resumable uploads use a session URI and chunk
  ranges.
- Azure block blobs upload named blocks and then commit a block list.

Pocopine should not expose those differences to Pine components. The
framework protocol should define the session, capability, progress,
resume, completion, and abort semantics; adapters then map those semantics
onto the storage engine.

This mirrors the sync architecture: `pocopine-sync` owns the protocol
contract between browser state and backend sources, while adapters provide
the persistence or change-source details. `pocopine-storage` should do the
same for upload sessions and object stores.

Pocopine already has separate concepts named "storage":

- `pocopine::storage::LocalStorage<T>` is a typed wrapper around browser
  `localStorage` for small client preferences.
- `pocopine-sync` has local stores for offline data and pending mutation
  replay.

This RFC is about neither of those. `pocopine-storage` is for binary
objects and upload/download workflows. It should compose with auth, server
plugins, deploy metadata, sync, and Pine UI components, but it remains an
explicit extension crate.

## 3. Goals

- Define `pocopine-storage` as an explicit extension crate, not a core
  `pocopine` feature.
- Keep the storage provider behind a trait so app code and Pine components
  do not depend on AWS, R2, MinIO, Supabase, local filesystem, or any other
  provider SDK.
- Make `StorageBackend` a supported public adapter contract so users can
  implement their own storage engine without forking Pine components or the
  browser protocol.
- Define a framework upload-session protocol that supports resumable
  chunked uploads and multipart part uploads from the first implementation
  slice.
- Install through the existing extension contracts: `ServerPlugin` on the
  host side and `AppPlugin` on the browser side.
- Provide a guarded server plugin that owns upload authorization, key
  generation, metadata validation, offset/part validation, completion,
  abort, signed download, and delete.
- Provide a browser `StorageClient` with typed upload sessions, progress,
  cancellation, retry, resume, and stable completion metadata.
- Define shared policy descriptors so Pine upload primitives can show the
  same limits that the backend enforces.
- Support both direct-to-provider uploads through signed targets and
  framework-proxied uploads when a backend cannot or should not expose a
  direct target.
- Return a provider-neutral `ObjectRef` that applications can store in
  their own database rows or sync payloads.
- Make public/private object visibility explicit. Private is the default.
- Make abandoned upload cleanup part of the server/backend contract.

## 4. Non-goals

- No database or ORM layer. Applications decide where to store `ObjectRef`
  values and domain metadata.
- No replacement for browser `LocalStorage<T>`.
- No replacement for `pocopine-sync` local stores or offline mutation
  replay.
- No global public bucket by default. Public files require an explicit
  policy.
- No provider SDK in user application code for the common path. Provider
  setup lives in adapter crates and app initialization.
- No requirement that every adapter expose every transfer strategy. The
  protocol advertises capabilities; the browser client chooses the best
  available strategy for the scope and file.
- No new plugin framework. Storage must use RFC 076 and RFC 077 rather
  than defining a storage-specific extension lifecycle.
- No promise that every storage provider can be made fully resumable.
  Direct single-request uploads remain non-resumable.
- No virus scanning or media transcoding in v1. The policy reserves hooks
  for scanning and post-processing, but those are later storage pipeline
  features.

## 5. Concepts

### 5.1 Protocol position

`pocopine-storage` should define its own framework protocol rather than
adopting one provider protocol wholesale.

The protocol should borrow the proven ideas from existing systems:

- [tus](https://tus.io/protocols/resumable-upload): upload creation,
  offset retrieval, `PATCH` append, expiration, checksums, termination,
  and concatenation extensions.
- [IETF HTTP resumable-upload draft](https://datatracker.ietf.org/doc/draft-ietf-httpbis-resumable-upload/):
  upload resources, `Upload-Offset`, `Upload-Complete`, offset mismatch
  errors, and retry after interrupted appends.
- [S3 multipart upload](https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html):
  create multipart upload, upload numbered parts, record part ETags or
  checksums, complete with the ordered part list, and abort abandoned
  multipart uploads.
- [Google Cloud Storage resumable uploads](https://cloud.google.com/storage/docs/performing-resumable-uploads):
  session URI, chunk ranges, and resume based on the server-reported range.
- [Azure block blobs](https://learn.microsoft.com/en-us/rest/api/storageservices/put-block)
  plus [Put Block List](https://learn.microsoft.com/en-us/rest/api/storageservices/put-block-list):
  upload blocks independently and commit the ordered block list.

The public Pocopine model distinguishes two transfer families:

1. **Sequential resumable upload**: bytes append in order. Resume is based
   on the current byte offset. This maps well to tus, the HTTP draft, GCS
   resumable uploads, local filesystem staging, and proxy streaming.
2. **Multipart upload**: chunks are independent parts. Parts may upload
   concurrently when the backend allows it. Completion uses the ordered
   list of part receipts. This maps well to S3 multipart and Azure block
   blobs.

Pine and application code ask for `UploadStrategy::Auto` by default. The
server returns the strategy and limits chosen for that file and scope.

The protocol is not wire-compatible with tus or the IETF draft. It borrows
their offset semantics for the sequential strategy, then adds multipart
part receipts so S3-compatible and Azure-style stores fit under the same
UI/API contract.

### 5.2 Backend

A backend is a named storage provider implementation:

```rust
pub type StorageBoxFuture<'a, T> =
    Pin<Box<dyn Future<Output = StorageResult<T>> + Send + 'a>>;

#[non_exhaustive]
pub enum StorageActor {
    Principal(PrincipalRef),
    Anonymous(AnonymousUploadBinding),
    System(&'static str),
}

pub struct StorageContext {
    pub actor: StorageActor,
    pub request: Option<RequestContext>,
}

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

    fn prepare_transfer<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: PrepareTransfer,
    ) -> StorageBoxFuture<'a, TransferTarget>;

    fn commit_transfer<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CommitTransfer,
    ) -> StorageBoxFuture<'a, UploadSession>;

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, ObjectRef>;

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()>;

    fn signed_read<'a>(
        &'a self,
        ctx: &'a StorageContext,
        object: &'a ObjectRef,
        options: ReadOptions,
    ) -> StorageBoxFuture<'a, SignedRead>;

    fn public_url<'a>(
        &'a self,
        object: &'a ObjectRef,
    ) -> StorageBoxFuture<'a, Option<String>>;

    fn proxy_read<'a>(
        &'a self,
        ctx: &'a StorageContext,
        object: &'a ObjectRef,
        options: ReadOptions,
    ) -> StorageBoxFuture<'a, StorageReadStream>;

    fn write_object<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: ServerWriteObject,
    ) -> StorageBoxFuture<'a, ObjectRef>;

    fn adopt_existing<'a>(
        &'a self,
        ctx: &'a StorageContext,
        object: ExistingObject,
    ) -> StorageBoxFuture<'a, ObjectRef>;

    fn delete_object<'a>(
        &'a self,
        ctx: &'a StorageContext,
        object: &'a ObjectRef,
    ) -> StorageBoxFuture<'a, ()>;

    fn cleanup_expired_uploads<'a>(
        &'a self,
        ctx: &'a StorageContext,
        scope: &'a str,
    ) -> StorageBoxFuture<'a, CleanupReport>;
}
```

The trait is intentionally provider-neutral and public. Pocopine will ship
first-party backends, but user crates may implement `StorageBackend` for
their own storage engines. The trait remains semver-governed after this
RFC is accepted; provider-specific crates should test against the shared
contract suite.

Backends translate this trait into S3 presigned PUTs, multipart sessions,
local filesystem writes, memory objects, database BLOBs, provider SDK
calls, or proxy routes.

`prepare_transfer` and `commit_transfer` are where engines differ. A
local filesystem backend may return a proxy append target and commit as
soon as bytes are durably written. An S3 backend may return a presigned
`UploadPart` URL and then commit the ETag returned by the browser. An
Azure backend may return a signed `Put Block` URL and then commit a block
ID. The browser sees the same `TransferTarget` shape either way.

`StorageContext` separates ordinary request work from system work. Routes
build a `StorageContext` from `RequestContext` after scope guard
authorization. Jobs and cleanup tasks use `StorageActor::System`, so
expired multipart aborts and server-side exports do not need to fake an
HTTP request.

### 5.3 Scope

A scope is the public application-facing name that browser code uses:
`avatars`, `invoice_attachments`, `richtext_embeds`, `imports`.

Scopes are registered explicitly:

```rust
StorageServer::builder()
    .public_scope("marketing_assets", public_asset_policy())
    .guarded_scope("avatars", avatar_policy(), require_auth())
    .guarded_scope_with("account_docs", docs_policy(), |ctx| async move {
        let user = ctx.require_user()?;
        ensure_account_access(user)?;
        Ok(())
    });
```

The browser never receives bucket names, provider credentials, raw key
prefix templates, or a list of all scopes. It asks for one registered
scope and gets only the policy descriptor needed for selection and upload.

Each scope has separate write, read, and delete authorization policy. The
simple `guarded_scope(...)` helper uses the same guard for all three.
Each scope also has a `StorageKeyResolver`; this is the app-owned contract
that turns an authorized upload intent into a canonical storage key.
Advanced apps can split guards and provide a resolver explicitly:

```rust
StorageServer::builder().scope(
    "account_docs",
    StorageScope::builder(docs_policy())
        .write_guard(require_account_writer())
        .read_guard(require_account_reader())
        .delete_guard(require_account_owner())
        .key_resolver(AccountDocumentKeys)
        .build(),
);
```

Holding an `ObjectRef` is never read permission. Every private read runs
the scope's read guard. Every delete runs the delete guard.

### 5.4 Upload policy

An upload policy defines the server-authoritative rules for one scope:

```rust
pub struct UploadPolicy {
    pub backend: StorageBackendName,
    pub max_bytes: u64,
    pub allowed_content_types: ContentTypeSet,
    pub allowed_extensions: ExtensionSet,
    pub max_files_per_batch: u32,
    pub visibility: ObjectVisibility,
    pub checksum: ChecksumPolicy,
    pub expires_after: Duration,
    pub metadata_schema: MetadataSchema,
    pub resumable: bool,
    pub preferred_chunk_size: Option<u64>,
    pub min_part_size: Option<u64>,
    pub max_part_size: Option<u64>,
    pub max_parts: Option<u32>,
    pub max_concurrent_parts: u16,
    pub max_open_sessions_per_principal: u32,
    pub max_bytes_per_window: Option<ByteWindowLimit>,
}
```

The server enforces the full policy during initiate and complete. The
browser descriptor is a safe projection:

```rust
pub struct UploadPolicyDescriptor {
    pub scope: String,
    pub max_bytes: u64,
    pub allowed_content_types: Vec<String>,
    pub allowed_extensions: Vec<String>,
    pub max_files_per_batch: u32,
    pub supports_progress: bool,
    pub supports_abort: bool,
    pub supports_batch: bool,
    pub strategies: Vec<UploadStrategy>,
    pub preferred_chunk_size: Option<u64>,
    pub min_part_size: Option<u64>,
    pub max_part_size: Option<u64>,
    pub max_parts: Option<u32>,
    pub max_concurrent_parts: u16,
}
```

Pine components use the descriptor for accept attributes, help text,
disabled states, and client-side preflight errors. Client-side validation
is advisory; the server remains the authority.

The descriptor is a mechanical safe projection of the policy plus backend
capabilities. It should be generated by framework code, not hand-written by
adapters, so UI validation cannot drift from server enforcement.

### 5.5 Object reference

An object reference is the provider-neutral value applications store:

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObjectRef {
    pub backend: String,
    pub scope: String,
    pub key: String,
    pub version: Option<String>,
    pub etag: Option<String>,
    pub checksum: Option<ObjectChecksum>,
    pub content_type: Option<String>,
    pub size: u64,
    pub visibility: ObjectVisibility,
    pub metadata: BTreeMap<String, String>,
}
```

`ObjectRef` is not a public URL. Private object reads go through
`StorageClient::signed_read` or a server route that checks the current
`RequestContext`. Public objects can expose a stable URL only when the
scope policy says public objects are allowed and the backend returns one
from `public_url`.

### 5.6 Upload session

An upload session is the protocol object that ties UI state to backend
state:

```rust
pub struct UploadSession {
    pub id: UploadSessionId,
    pub scope: String,
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub strategy: UploadStrategy,
    pub status: UploadSessionStatus,
    pub next_offset: Option<u64>,
    pub part_size: Option<u64>,
    pub plan: TransferPlan,
    pub uploaded_parts: Vec<UploadedPartView>,
    pub expires_at: OffsetDateTime,
}

pub struct TransferPlan {
    pub min_part_size: Option<u64>,
    pub preferred_part_size: Option<u64>,
    pub max_part_size: Option<u64>,
    pub max_parts: Option<u32>,
    pub max_concurrent_parts: u16,
    pub resumable: bool,
}

#[non_exhaustive]
pub enum UploadStrategy {
    Auto,
    SingleRequest,
    Sequential,
    Multipart,
}

#[non_exhaustive]
pub enum UploadSessionStatus {
    Open,
    Completing,
    Complete,
    Aborted,
    Expired,
}
```

`UploadSession` is the browser-safe view. The server stores a
`StoredUploadSession` that additionally contains the owner binding,
provider upload id/session URI, server-only part receipts, and any backend
cleanup metadata:

```rust
pub struct StoredUploadSession {
    pub public: UploadSession,
    pub owner: UploadOwner,
    pub storage_key: StorageKey,
    pub provider_state: ProviderUploadState,
    pub part_receipts: BTreeMap<u32, ProviderPartReceipt>,
}

pub enum UploadOwner {
    Principal(PrincipalRef),
    Anonymous(AnonymousUploadBinding),
    System(&'static str),
}
```

Upload session ids must carry at least 128 bits of entropy. They are
bearer-like handles when persisted in browser storage, so every inspect,
transfer, complete, abort, read, and delete route revalidates that the
current `StorageContext.actor` matches the stored `UploadOwner`. Public
anonymous scopes bind sessions to a framework-issued anonymous upload
binding, such as a same-origin cookie plus CSRF token, not merely to the
object key or file name.

For sequential uploads, `next_offset` is authoritative. If the browser
thinks it has sent more bytes than the server has committed, it resumes
from the server offset.

For multipart uploads, each completed part has a provider-neutral receipt:

```rust
pub struct UploadedPartView {
    pub number: u32,
    pub offset: u64,
    pub size: u64,
    pub checksum: Option<ObjectChecksum>,
    pub status: UploadedPartStatus,
}
```

Provider receipts are server-only. They let adapters keep S3 ETags, Azure
block IDs, GCS upload state, or other completion tokens without leaking
provider details into Pine or synced app state.

### 5.7 Extension contracts

Storage installs through the existing Pocopine plugin framework.

Terminology matters here. `pocopine-storage` is an explicit extension
crate like `pocopine-sync`. Backend crates such as `pocopine-storage-s3`
implement `StorageBackend`; they are not separate app/server plugins unless
they need their own lifecycle hooks. This is different from per-runtime
component extension traits such as `pine-richtext::RichTextExtension`,
where authors compose editor behavior inside one mounted surface.

On the host side, `pocopine_storage::storage_server_plugin(storage)` is a
`ServerPlugin`. It must:

- override `name()` with the stable name `"pocopine-storage"`;
- `provide_plugin(storage.clone())` so other server integrations can reach
  the runtime service when needed;
- add its HTTP routes with `Server::route` or `Server::router_mut`;
- bind per-handler state with axum `State` / `with_state`, not repeated
  `active_plugin::<StorageServer>()` lookups on chunk hot paths;
- avoid installing tower layers unless a later concrete need appears.

Plugin installation remains synchronous. Expensive backend setup belongs in
app startup before the plugin value is constructed, or in backend-managed
lazy state. Upload-session state belongs to `StorageBackend` / `StorageServer`,
not to the server plugin registry; the registry is only the framework
service lookup.

On the browser side, `pocopine_storage::storage_plugin()` is an
`AppPlugin`. It must:

- override `name()` with `"pocopine-storage"`;
- `provide_plugin(StorageClient { ... })` as the runtime service;
- keep installation synchronous and deterministic;
- expose endpoint, credentials, and resume-cache configuration through the
  plugin builder.

The app plugin should mirror `pocopine-sync`: installing it provides the
client service, but it does not silently make every component upload-aware.
Components opt in by extracting the service.

Pine upload primitives should consume `Plugin<StorageClient>` or
`Option<Plugin<StorageClient>>` from component lifecycle/handler code. A
component that cannot upload without the storage client may require
`Plugin<StorageClient>` and fail loudly when the app forgot
`storage_plugin()`. Reusable components that can render a disabled or
manual state should use `Option<Plugin<StorageClient>>`.

## 6. Server API

### 6.1 Plugin mounting

Storage routes mount through the server plugin lifecycle:

```rust
#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_server::{axum::Router, serve, static_files, Server};
    use pocopine_storage::storage_server_plugin;

    let router = Router::new()
        .fallback_service(static_files(env!("CARGO_MANIFEST_DIR")));

    let router = Server::new(router)
        .plugin(storage_server_plugin(storage_server()))
        .with_auth(auth_provider())
        .try_finalize()
        .map_err(std::io::Error::other)?;

    serve(router, "127.0.0.1:3022").await
}
```

Routes live under `/__pocopine/storage/v1/...` and are not generated
`#[server]` functions. Large file bytes should not move through serde
JSON server-function payloads.

Because storage adds routes, install `storage_server_plugin(...)` before
route-wrapping layers such as `Server::with_auth(...)` and observability's
request event layer. This follows RFC 077's axum layer rule: layers only
wrap routes that already exist at the call site. Guarded storage scopes
that read the authenticated principal from `RequestContext` need the auth
middleware to wrap the storage routes.

The sync plugin is the model to follow: one extension crate owns the
server protocol routes, provides a runtime service, and leaves application
data/storage policy to registered sources and backends.

### 6.2 Routes

The initial route set:

| Route | Purpose |
|---|---|
| `GET /__pocopine/storage/v1/scopes/{scope}` | Return a safe policy descriptor for one scope after guard validation. |
| `POST /__pocopine/storage/v1/uploads` | Run the write guard, resolve a `StorageKey`, initiate one upload, and return an upload session with the chosen strategy and limits. |
| `GET /__pocopine/storage/v1/uploads/{session}` | Inspect resumable state: status, next offset, completed parts, expiry, and selected strategy. |
| `PUT /__pocopine/storage/v1/uploads/{session}/bytes` | Single-request proxy upload. Non-resumable; valid only for sessions whose selected strategy is `SingleRequest`. |
| `PATCH /__pocopine/storage/v1/uploads/{session}/bytes` | Sequential proxy append. Uses the session's current offset and rejects mismatches. |
| `POST /__pocopine/storage/v1/uploads/{session}/parts` | Prepare the next multipart transfer target, either direct provider target or framework proxy target. |
| `PUT /__pocopine/storage/v1/uploads/{session}/parts/{part}/bytes` | Optional proxy route for uploading one multipart part through the framework. |
| `POST /__pocopine/storage/v1/uploads/{session}/parts/{part}/complete` | Commit one part receipt after direct or proxy upload. |
| `POST /__pocopine/storage/v1/uploads/{session}/complete` | Validate all offsets or part receipts and return `ObjectRef`. |
| `DELETE /__pocopine/storage/v1/uploads/{session}` | Abort an upload session and release backend resources when possible. |
| `POST /__pocopine/storage/v1/read` | Return a signed read target for a private object after guard validation. |
| `GET /__pocopine/storage/v1/objects/{scope}/{key...}` | Optional proxy read route for private objects that must stream through app authorization instead of signed URLs. |
| `DELETE /__pocopine/storage/v1/objects` | Delete an object after guard validation and policy checks. |

Every route receives a `RequestContext`. Public scopes are explicit.
Guarded scopes run their guard before the backend sees the request.

Sequential append errors should use an explicit offset-mismatch response
with both the expected and provided offsets, mirroring tus and the HTTP
draft. The browser client must recover by inspecting the session and
resuming from the backend's committed offset.

Completion routes are idempotent:

- completing an already completed upload returns the same `ObjectRef`;
- aborting an already aborted or expired upload is a no-op;
- committing the same part with the same receipt is a no-op;
- committing the same part with a different receipt is `PartRejected`.

The framework may use `(session, part, receipt_hash)` for part idempotency
and `(session, complete_request_hash)` for final completion. Backends that
need provider idempotency keys can derive them from those stable values.

### 6.3 Upload target modes

Backends choose one of two target modes per session:

```rust
pub enum UploadTarget {
    Direct {
        method: HttpMethod,
        url: String,
        headers: Vec<(String, String)>,
        expires_at: OffsetDateTime,
    },
    Proxy {
        url: String,
        headers: Vec<(String, String)>,
    },
}
```

Direct mode is preferred for object stores: the browser uploads bytes to
the provider's signed URL and only sends completion metadata back to the
Pocopine server.

Proxy mode is for development backends, local filesystem storage, or
providers that require server-side streaming. The framework route should
stream bytes; it must not buffer the full file in memory.

For multipart uploads, a transfer target also carries part identity:

```rust
pub struct TransferTarget {
    pub session: UploadSessionId,
    pub strategy: UploadStrategy,
    pub part: Option<UploadPartNumber>,
    pub offset: Option<u64>,
    pub size: Option<u64>,
    pub target: UploadTarget,
    pub checksum: ChecksumPolicy,
}
```

Not every strategy/target pair is resumable:

| Strategy | Target | Resumable | Notes |
|---|---|---|---|
| `SingleRequest` | `Proxy` | No | Simple small-file path. Retry starts over. |
| `SingleRequest` | `Direct` | No | A single presigned PUT has no portable resume state. |
| `Sequential` | `Proxy` | Yes | Resume from `UploadSession.next_offset`. |
| `Sequential` | `Direct` | Provider-dependent | Valid only when the backend can reprepare an offset-aware target, such as GCS-style sessions. |
| `Multipart` | `Proxy` | Yes | Retry failed parts independently. |
| `Multipart` | `Direct` | Yes | Re-sign failed part targets and commit server-side receipts. |

If `UploadPolicy.resumable` is true, `UploadStrategy::Auto` must not choose
`SingleRequest` for files above the configured small-file threshold. The
backend computes the authoritative transfer plan during `initiate_upload`,
including min part size, max part size, max part count, and concurrency.

### 6.4 Key ownership

The browser never chooses the final key. The storage framework also should
not encode app domain concepts such as principal, account, project,
invoice, or workspace. Those are application concepts. The scope hands key
resolution to an app-owned resolver:

```rust
#[async_trait]
pub trait StorageKeyResolver: Send + Sync + 'static {
    async fn resolve_key(
        &self,
        ctx: &StorageContext,
        intent: &UploadIntent,
    ) -> StorageResult<StorageKey>;
}
```

The resolver receives an authorized `StorageContext` and a framework-built
`UploadIntent`. It returns a `StorageKey`, not a raw string:

```rust
pub struct StorageKey {
    pub key: SafeObjectKey,
    pub owner: Option<ObjectOwnerRef>,
    pub metadata: ObjectMetadata,
}
```

`StorageKey` is the handoff point between app meaning and provider storage.
The app owns naming and ownership metadata. Pocopine owns key safety,
session binding, backend dispatch, resumability, and the final `ObjectRef`.
Initiating an upload therefore has a fixed order: run the scope write
guard, build `UploadIntent`, call `StorageKeyResolver::resolve_key`,
validate the returned `SafeObjectKey`, pass `InitiateUpload { storage_key,
.. }` to the backend, then store the same `StorageKey` in the server-side
upload session.

An app resolver can be as simple as a prefix formatter, but the framework
does not need a prefix enum to model that:

```rust
pub struct AvatarStorageKeys;

#[async_trait]
impl StorageKeyResolver for AvatarStorageKeys {
    async fn resolve_key(
        &self,
        ctx: &StorageContext,
        intent: &UploadIntent,
    ) -> StorageResult<StorageKey> {
        let principal = ctx.require_principal()?;
        let object_id = intent.generated_object_id();
        let extension = intent.extension().unwrap_or("");

        let key = SafeObjectKey::parse(format!(
            "avatars/{}/{}{}",
            principal.subject,
            object_id,
            extension,
        ))?;

        Ok(StorageKey {
            key,
            owner: Some(ObjectOwnerRef::principal(principal.subject)),
            metadata: ObjectMetadata::from([
                ("kind", "avatar"),
                ("original_name", intent.file_name()),
            ]),
        })
    }
}
```

Other resolvers may load a domain record, allocate a database id, use an
import job id, or choose a different prefix for public and private objects.
That logic belongs to the application struct implementing
`StorageKeyResolver`, not to `pocopine-storage`.

```rust
pub struct SafeObjectKey(String);

impl SafeObjectKey {
    pub fn parse(key: &str) -> StorageResult<Self>;
}
```

`SafeObjectKey` rejects absolute paths, `..`, empty segments, control
characters, provider-reserved prefixes, and cross-scope prefixes.
Client-provided file names are metadata only unless the resolver explicitly
includes a sanitized form.

### 6.5 Server-side and worker writes

Browser upload is not the only storage use case. Server functions and RFC
067 jobs need to write generated reports, exports, imports, thumbnails, and
remote fetch results without a `web_sys::File`.

`StorageServer` should expose a host-side API:

```rust
impl StorageServer {
    pub async fn write_object(
        &self,
        ctx: StorageContext,
        scope: &str,
        request: ServerWriteObject,
    ) -> StorageResult<ObjectRef>;

    pub async fn open_writer(
        &self,
        ctx: StorageContext,
        scope: &str,
        request: ServerWriteObject,
    ) -> StorageResult<StorageWriteStream>;

    pub async fn adopt_existing(
        &self,
        ctx: StorageContext,
        scope: &str,
        object: ExistingObject,
    ) -> StorageResult<ObjectRef>;
}

pub struct ServerWriteObject {
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub size_hint: Option<u64>,
    pub metadata: BTreeMap<String, String>,
    pub body: StorageBody,
}
```

Server-side writes run the same scope policy and key resolver as browser
uploads, but they use `StorageActor::System` or the server function's
authenticated principal. A job that writes an export should produce the
same `ObjectRef` shape as a browser upload, so application rows and sync
payloads do not care how the object was created.

## 7. Browser API

The browser client is installed by an app plugin and uses the same public
URL conventions as other Pocopine client helpers, but it is not a
server-function stub.

```rust
pocopine::app! {
    components: [AppShell, AvatarForm],
    plugins: [
        pocopine_storage::storage_plugin()
            .endpoint("/__pocopine/storage/v1")
            .with_credentials(true),
    ],
    routes: [("/", AvatarForm)],
}
```

Components and Pine primitives should use the runtime service:

```rust
fn on_ready(&mut self, storage: Plugin<StorageClient>) {
    self.storage = Some(storage);
}
```

Applications that are outside a component context can still construct an
explicit client for scripts/tests, but Pine's default path is the
installed `StorageClient` service so endpoint, credentials, and resume
cache policy are app-owned.

```rust
let client = pocopine_storage::StorageClient::new();

let descriptor = client.scope("avatars").descriptor().await?;

let upload = client
    .scope("avatars")
    .upload(file)
    .strategy(UploadStrategy::Auto)
    .metadata("alt", "Profile photo")
    .on_progress(|progress| {
        tracing::debug!(sent = progress.bytes_sent, total = progress.bytes_total);
    })
    .send()
    .await?;

let object: ObjectRef = upload.object;
```

The public browser surface:

```rust
pub struct StorageClient;
pub struct StorageScopeClient;
pub struct UploadBuilder;
pub struct UploadTask;

impl StorageClient {
    pub fn new() -> Self;
    pub fn scope(&self, scope: impl Into<String>) -> StorageScopeClient;
}

impl StorageScopeClient {
    pub async fn descriptor(&self) -> Result<UploadPolicyDescriptor, StorageError>;
    pub fn upload(&self, file: web_sys::File) -> UploadBuilder;
    pub fn upload_blob(&self, blob: web_sys::Blob, name: impl Into<String>) -> UploadBuilder;
    pub async fn session(&self, id: UploadSessionId) -> Result<UploadSession, StorageError>;
    pub fn resume(&self, file: web_sys::File, session: UploadSession) -> UploadBuilder;
    pub async fn signed_read(&self, object: ObjectRef) -> Result<SignedRead, StorageError>;
    pub async fn delete(&self, object: ObjectRef) -> Result<(), StorageError>;
}
```

The browser client owns the strategy-specific transfer loop:

- `SingleRequest` uploads one body and completes.
- `Sequential` inspects the session, slices the file at `next_offset`,
  appends one chunk at a time, and recovers from offset mismatches.
- `Multipart` prepares part targets, uploads one or more parts, records
  receipts, retries failed parts independently, and completes with the
  ordered receipt list.
- `Auto` chooses the server-preferred strategy from the policy descriptor
  and file size.

The upload task exposes progress, retry, resume, and cancellation in a
component-friendly shape:

```rust
pub struct UploadProgress {
    pub bytes_sent: u64,
    pub bytes_total: Option<u64>,
    pub current_part: Option<u32>,
    pub phase: UploadPhase,
}

pub enum UploadPhase {
    Initiating,
    Uploading,
    Retrying,
    Completing,
    Complete,
    Aborted,
    Failed,
}
```

The client may persist `(scope, session_id, file_name, size,
last_modified)` in browser storage so an app can offer resume after a
reload. It must not persist provider URLs as durable state; signed targets
expire and must be re-prepared through the server.

Long uploads must tolerate auth changes. If a token refresh or session
rotation changes the credential material but not the authenticated
principal, the client may continue after the next route call revalidates.
If the principal changes, the session is no longer resumable by that
browser context and the client reports `Unauthorized`.

## 8. Pine integration

Pine should not know about S3, R2, local filesystem, or signed URL
formats. It should consume `pocopine-storage` browser APIs.

### 8.1 Low-level upload utility

The first Pine-facing surface should be a small utility layer:

```rust
// In pine::upload.
pub mod upload {
    pub use pocopine_storage::{
        ObjectRef, StorageClient, StorageError, UploadItem, UploadPhase, UploadProgress,
    };

    pub async fn upload_files(
        scope: &str,
        files: Vec<web_sys::File>,
    ) -> Result<Vec<UploadItem>, StorageError>;
}
```

This lets application authors build custom file inputs without waiting
for a full dropzone component. Component-owned utilities should prefer the
installed `StorageClient` service over constructing a default client.

### 8.2 Dropzone primitives

The component layer should be unstyled and composable, matching Pine's
primitive direction:

- `PineDropzoneRoot`
- `PineDropzoneInput`
- `PineDropzoneTrigger`
- `PineDropzoneList`
- `PineDropzoneItem`
- `PineDropzoneRemove`

`PineDropzoneRoot` owns selection state, descriptor loading, validation,
upload session state, chunk/part progress, resume prompts, retry actions,
and event emission:

```html
<pine-dropzone-root
  scope="avatars"
  auto-upload
  resumable
  pp-model:items="avatar_uploads"
  @storage-complete="avatar_uploaded"
  @storage-error="avatar_upload_failed"
>
  <pine-dropzone-input />
  <pine-dropzone-trigger />
  <pine-dropzone-list />
</pine-dropzone-root>
```

The model value is a list of `UploadItem` values:

```rust
pub struct UploadItem {
    pub client_id: String,
    pub session_id: Option<UploadSessionId>,
    pub file_name: String,
    pub size: u64,
    pub content_type: Option<String>,
    pub strategy: Option<UploadStrategy>,
    pub phase: UploadPhase,
    pub progress: Option<UploadProgress>,
    pub object: Option<ObjectRef>,
    pub error: Option<String>,
}
```

Events use Pocopine's author-facing `payload` vocabulary:

```rust
pub struct StorageCompletePayload {
    pub item: UploadItem,
    pub object: ObjectRef,
}

pub struct StorageErrorPayload {
    pub item: UploadItem,
    pub error: StorageError,
}
```

### 8.3 Why utility first

The utility should land before the full dropzone component. It pins the
browser/backend contract and lets early applications build their own UI.
The dropzone then becomes a consumer of a proven upload state machine,
not a place where backend semantics are invented.

The UI contract is deliberately storage-agnostic. A failed S3 part upload,
a mismatched sequential offset, and an expired GCS session all become
typed `UploadItem` state that Pine can render consistently.

### 8.4 Sync interop

`ObjectRef` must be `Serialize` / `Deserialize` and stable inside
`pocopine-sync` rows. Object lifecycle is independent of row lifecycle:
deleting a row that contains an `ObjectRef` does not automatically delete
the object. Apps that want coupled cleanup wire that policy explicitly in
their server functions, jobs, or resource hooks.

Sync-aware UIs often need to show an attachment before upload completion.
Use a typed placeholder rather than inventing partial object refs:

```rust
pub enum StorageRef {
    Pending { session_id: UploadSessionId, scope: String },
    Ready(ObjectRef),
}
```

A pending storage ref may be synced as optimistic local state, but the
server should only persist `Ready(ObjectRef)` for durable rows unless the
resource explicitly supports pending uploads. If two devices race to
upload the same logical attachment, the application resource resolves the
conflict; storage keys remain server-generated and unique by default.

### 8.5 Rich-text embeds

Rich-text image/file embeds should store `ObjectRef` or `StorageRef`, not
provider URLs. Rendering a private image asks `StorageClient` for a
signed-read URL or uses the proxy read route. Public embeds may use the
backend `public_url` only when the scope is public.

## 9. Security model

- Private visibility is the default. Public files require an explicit
  scope policy.
- Scope guards run before descriptor, initiate, inspect, transfer,
  complete, read, delete, and abort.
- Upload sessions are bound to the initiating principal, anonymous upload
  binding, or system actor and are revalidated on every session route.
- The server-side key resolver chooses keys. The browser cannot submit
  arbitrary bucket names, prefixes, provider URLs, or final keys.
- Signed direct-upload targets are short lived and bound to the initiated
  session.
- Completion revalidates the session, size, content type, checksum when
  configured, and provider-reported object identity.
- Client-provided content type is advisory. Backends should preserve the
  browser-provided value only after policy validation, and adapters may
  add provider-side content-type or checksum assertions where available.
- Reads for private objects are authorized independently of writes. Having
  an `ObjectRef` is not permission to read it.
- Deletes are explicit operations. Application-level row deletion does
  not automatically delete the object unless the app wires that policy.
- Abandoned sessions must expire. Backends with multipart resources must
  abort expired sessions through provider cleanup or a scheduled job.
- Proxy upload and delete routes are state-changing same-origin requests.
  Apps that use cookie auth must use the same CSRF strategy required by
  their server-function/auth stack; storage routes should reject unsafe
  cookie-authenticated requests that lack the configured CSRF proof.
- Policies should include quota/rate limits for open sessions and bytes
  per time window. Expiry alone is not enough to prevent multipart quota
  exhaustion.

## 10. Error and failure model

All public operations return `Result<_, StorageError>`.

```rust
#[non_exhaustive]
pub enum StorageError {
    Unauthorized,
    ScopeNotFound,
    PolicyRejected(PolicyRejection),
    QuotaExceeded(QuotaRejection),
    CsrfRequired,
    BackendUnavailable,
    OffsetMismatch { expected: u64, provided: u64 },
    PartRejected { part: u32, reason: String },
    UploadExpired,
    UploadAborted,
    ChecksumMismatch,
    Provider(String),
    Network(String),
    InvalidResponse(String),
}
```

The browser distinguishes:

- policy rejection before upload starts,
- sequential offset mismatch during resume,
- failed multipart part preparation or commit,
- network/provider failure while uploading bytes,
- completion failure after bytes may already exist,
- abort success/failure,
- read/delete authorization failure.

If completion fails after a direct provider upload, the client must not
invent an `ObjectRef`. The server returns either a valid completed object
or an error. Cleanup of orphaned provider objects belongs to session
expiry and backend cleanup.

### 10.1 Protocol type definitions

The first implementation should define these small protocol types instead
of leaving them as implicit placeholders:

```rust
pub type StorageResult<T> = Result<T, StorageError>;

pub struct StorageBackendName(pub String);
pub struct UploadSessionId(pub String);
pub struct ContentTypeSet(Vec<mime::Mime>);
pub struct ExtensionSet(Vec<String>);
pub struct PrincipalRef {
    pub subject: String,
    pub attributes: BTreeMap<String, String>,
}
pub struct AnonymousUploadBinding {
    pub id: String,
}
pub struct UploadIntent {
    pub scope: String,
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub requested_strategy: UploadStrategy,
    pub generated_object_id: String,
}
pub struct StorageKey {
    pub key: SafeObjectKey,
    pub owner: Option<ObjectOwnerRef>,
    pub metadata: ObjectMetadata,
}
pub struct ObjectOwnerRef {
    pub kind: String,
    pub id: String,
}
pub struct ObjectMetadata(pub BTreeMap<String, String>);
pub struct SafeObjectKey(String);

impl SafeObjectKey {
    pub fn parse(key: &str) -> StorageResult<Self>;
}
pub struct MetadataSchema {
    pub allowed_keys: Vec<String>,
    pub max_key_bytes: usize,
    pub max_value_bytes: usize,
}
pub struct ByteWindowLimit {
    pub max_bytes: u64,
    pub window: Duration,
}
pub struct QuotaRejection {
    pub reason: &'static str,
    pub retry_after: Option<Duration>,
}

#[non_exhaustive]
pub enum ObjectVisibility {
    Private,
    Public,
}

#[non_exhaustive]
pub enum UploadedPartStatus {
    Prepared,
    Uploaded,
    Committed,
}

pub enum PolicyRejection {
    TooLarge { max_bytes: u64 },
    ContentTypeRejected,
    ExtensionRejected,
    MetadataRejected(&'static str),
    StrategyUnavailable,
}

#[non_exhaustive]
pub enum ChecksumPolicy {
    None,
    Optional(Vec<ChecksumAlgorithm>),
    Required(ChecksumAlgorithm),
}

#[non_exhaustive]
pub enum ChecksumAlgorithm {
    Sha256,
    Crc32c,
    Md5,
}

pub struct ObjectChecksum {
    pub algorithm: ChecksumAlgorithm,
    pub value: String,
}

pub struct SignedRead {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub expires_at: OffsetDateTime,
}
pub struct StorageReadStream {
    pub content_type: Option<String>,
    pub size: Option<u64>,
    pub body: StorageBody,
}
pub struct StorageWriteStream {
    pub session: UploadSessionId,
    pub body: StorageBody,
}
pub enum StorageBody {
    Bytes(Vec<u8>),
    Stream(Pin<Box<dyn Stream<Item = Result<Bytes, StorageError>> + Send>>),
}

pub struct ReadOptions {
    pub disposition: Option<ContentDisposition>,
    pub max_age: Option<Duration>,
}

#[non_exhaustive]
pub enum ContentDisposition {
    Inline,
    Attachment { filename: Option<String> },
}

pub struct UploadPartNumber(pub u32);

#[non_exhaustive]
pub enum StorageBackendKind {
    Memory,
    LocalFs,
    S3Compatible,
    Gcs,
    AzureBlob,
    Custom(String),
}

pub struct CleanupReport {
    pub expired_sessions: u64,
    pub aborted_provider_uploads: u64,
    pub errors: u64,
}

pub struct ProviderPartReceipt {
    pub adapter: String,
    pub bytes: Bytes,
}

pub enum ProviderUploadState {
    Memory,
    LocalFs { path: PathBuf },
    Adapter(Bytes),
}

pub struct InitiateUpload {
    pub scope: String,
    pub storage_key: StorageKey,
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub requested_strategy: UploadStrategy,
}

pub struct PrepareTransfer {
    pub session: UploadSessionId,
    pub part: Option<UploadPartNumber>,
    pub offset: Option<u64>,
    pub size: u64,
}

pub struct CommitTransfer {
    pub session: UploadSessionId,
    pub part: Option<UploadPartNumber>,
    pub offset: Option<u64>,
    pub size: u64,
    pub checksum: Option<ObjectChecksum>,
    pub provider_headers: Vec<(String, String)>,
}

pub struct CompleteUpload {
    pub session: UploadSessionId,
    pub checksum: Option<ObjectChecksum>,
}

pub struct ExistingObject {
    pub key: SafeObjectKey,
    pub size: Option<u64>,
    pub content_type: Option<String>,
}
```

These shapes can still evolve while the RFC is Draft, but reviewers should
not have to infer whether they are strings, enums, or opaque provider
values.

## 11. Observability

Storage should emit framework telemetry through the existing Pocopine
targets:

- `pocopine.log` for policy rejection, provider errors, cleanup failures,
  and unsafe request rejection;
- `pocopine.trace` for session lifecycle, retry, abort, and route timing;
- `pocopine.metric` for bytes uploaded, completion latency, error class,
  active sessions, expired sessions, and proxy-vs-direct transfer counts.

Events must not include object bytes, signed URLs, provider credentials,
raw metadata values, cookies, authorization headers, or client file paths.
Provider-specific adapter crates may add richer metrics, but the base
contract should give operators enough signal to see upload health.

## 12. Deployment and configuration

`pocopine-storage` should emit build/deploy metadata that RFC 080 can
consume:

```rust
pub struct StorageDeployMetadata {
    pub uses_object_storage: bool,
    pub backends: Vec<StorageBackendRequirement>,
}

pub struct StorageBackendRequirement {
    pub name: String,
    pub kind: StorageBackendKind,
    pub requires_bucket: bool,
    pub required_env: Vec<String>,
}
```

The deploy layer can then warn when an app uses storage but no object
storage configuration is present. It should not provision buckets in v1.
Provider credentials remain host secrets.

Provider adapters must document direct-upload CORS. For S3-compatible
backends that means listing the required methods, headers, exposed ETag or
checksum headers, allowed origin shape, and max age. Missing CORS config is
an onboarding failure, not an advanced tuning concern.

Local development should work without a cloud account:

```rust
let backend = pocopine_storage::fs::LocalFsBackend::new("./.pocopine/storage");
```

The local filesystem adapter is host-only and should refuse path escapes.
The memory adapter is for tests and demos.

The v1 policy points each scope at one backend by name. Multi-region and
multi-bucket routing can be added later by letting a scope choose a backend
from authorized app context before backend initiation. The initial contract
should not bake region into `ObjectRef` except through the backend name and
opaque key.

Direct upload and proxy upload have different cost surfaces. Direct upload
saves server bandwidth and usually scales better. Proxy upload keeps every
byte under application auth, audit, transformation, and same-origin policy,
but it consumes server bandwidth and can increase host costs. The strategy
selection should be explicit in policy and visible in metrics.

## 13. Implementation plan

### Phase 1 - Protocol, local backend, browser client

- Add `pocopine-storage` crate.
- Define provider-neutral protocol structs, `ObjectRef`, `UploadPolicy`,
  `UploadPolicyDescriptor`, `StorageKey`, `StorageKeyResolver`,
  `UploadSession`, `TransferTarget`, `UploadedPartView`, server-only part
  receipts, `TransferPlan`, and `StorageError`.
- Add `StorageBackend` trait with session inspection, transfer
  preparation, transfer commit, upload completion, server-side writes,
  cleanup, read, and delete.
- Add memory and local filesystem backends that implement sequential
  resumable uploads through proxy streaming.
- Add `StorageServerPlugin` / `storage_server_plugin(...)` using the RFC
  077 contract: stable plugin name, `provide_plugin`, routes with
  `with_state`, and tests for layer/auth ordering guidance.
- Add server plugin routes for descriptor, initiate, inspect, sequential
  append, single-request upload, multipart transfer preparation, part
  commit, complete, abort, signed read, proxy read, and delete.
- Add `StorageClient`, `UploadBuilder`, resumable session inspection,
  sequential chunk loop, direct/proxy single-request loop, upload progress,
  cancellation, retry, resume, and direct/proxy target handling.
- Add `StorageClientPlugin` / `storage_plugin()` using the RFC 076
  contract: stable plugin name, synchronous install, app-owned endpoint,
  credentials mode, and resume-cache configuration.
- Add host tests that cover guard enforcement, policy rejection, key
  resolution, session/principal binding, offset mismatch recovery,
  idempotent complete/abort, completion validation, read authorization,
  proxy read, server-side writes, cleanup, quota rejection, and CSRF
  rejection.
- Add wasm tests for descriptor load, policy rejection, proxy sequential
  upload, resume from offset, retry after interrupted chunk, completion
  state, abort, and expired-session recovery.
- Add one end-to-end example or integration test that creates a session,
  uploads through the browser client, completes to `ObjectRef`, and reads
  it back through the configured read path.
- Document the contract in `docs/storage.md`.

Phase 1 should land as one merge unit. The server protocol should not land
without a real browser consumer because the contract needs a round-trip
test before it becomes reference material for backend authors.

### Phase 2 - S3-compatible adapter and multipart e2e

- Add `pocopine-storage-s3` with AWS S3-compatible presigned upload/read
  support and multipart upload mapping.
- Verify AWS S3, Cloudflare R2, and MinIO against the same trait tests
  where possible.
- Keep provider configuration in the adapter crate and app initialization.
- Add direct multipart browser tests with provider fakes and at least one
  MinIO-style integration path in CI when credentials are available.

### Phase 3 - Pine upload utility and dropzone

- Add `pine::upload` utility surface.
- Add unstyled dropzone primitives that consume `pocopine-storage`.
- Ensure Pine upload components use `Plugin<StorageClient>` /
  `Option<Plugin<StorageClient>>` rather than constructing provider-specific
  clients.
- Add resumable UI states: paused, retrying, resumable-after-reload,
  expired, and per-part progress.
- Add website examples for avatar upload, multi-file attachments, and
  private download preview.

### Phase 4 - More backends and pipelines

- Add GCS resumable-upload and Azure block-blob adapters.
- Add scanning/post-processing hooks.
- Add scheduled cleanup for expired sessions where the provider needs an
  active abort.
- Add richer object lifecycle helpers once real applications prove the
  required shape.

## 14. Migration

Existing apps keep using their current upload routes. There is no
breaking change to `pocopine::storage::LocalStorage<T>` or
`pocopine-sync`.

Apps can migrate one workflow at a time:

1. Add `pocopine-storage` and a backend adapter.
2. Register a scope that matches the existing route's policy.
3. Swap custom browser upload code to `StorageClient`.
4. Store returned `ObjectRef` values in the existing application table.
5. Optionally replace custom UI with Pine upload/dropzone primitives.

Apps that already have bucket objects can adopt them without re-uploading:

```rust
let object = storage
    .adopt_existing(
        ctx,
        "avatars",
        ExistingObject {
            key: SafeObjectKey::parse("avatars/u1/old.png")?,
            size: None,
            content_type: Some("image/png".to_string()),
        },
    )
    .await?;
```

Adoption still runs scope policy, validates key safety, and records a normal
`ObjectRef`. It does not grant read permission beyond the scope's read
guard.

## 15. Drawbacks

- A framework-level storage contract is more API surface to maintain.
- Upload providers differ in subtle ways: signed header behavior, multipart
  completion, checksum support, object versioning, and public URL rules.
- Direct upload means the browser talks to another origin, so CORS setup
  becomes part of the adapter documentation.
- Returning `ObjectRef` does not solve application lifecycle policy. Apps
  still need to decide when object deletion follows row deletion.
- Proxy uploads are simpler but can consume server bandwidth and must be
  streamed carefully.
- A public backend trait raises compatibility expectations. The contract
  suite and `#[non_exhaustive]` protocol enums are necessary to evolve the
  trait without breaking every user backend.

## 16. Alternatives

### 16.1 Use only server functions

Generated `#[server]` functions are good for typed JSON commands. They
are the wrong transport for large binary bodies. Upload routes need
streaming, progress, direct provider targets, and completion semantics.

### 16.2 Make PineDropzone accept an arbitrary upload callback

This is flexible but does not solve policy drift. The component would not
know the backend's max size, content types, auth state, or completion
metadata shape. The callback escape hatch can still exist, but the default
path should be `pocopine-storage`.

### 16.3 Build only an S3 integration

S3-compatible storage is the likely first production adapter, but baking
S3 into the UI or app-facing API would make local dev, tests, Supabase,
R2, MinIO, GCS, and Azure harder than necessary. The framework contract
should stay provider-neutral.

### 16.4 Store files in the application database

Some apps need database BLOBs. That can be implemented as a backend
adapter, but it should not be the default. Most production apps want
object storage for large files, CDN integration, signed reads, and
lifecycle policies.

### 16.5 Adopt tus as the public API

tus is the strongest existing resumable-upload ecosystem and should
influence the Pocopine sequential strategy. Adopting tus wholesale would
still leave S3 multipart, Azure block lists, and provider-specific signed
URL completion outside the model. Pocopine needs one API that can drive
both offset append and multipart part receipts.

### 16.6 Expose each provider protocol directly

This keeps adapters thin but pushes the hard work to every Pine component
and app. The UI would need to know tus offsets, S3 ETags, GCS ranges, and
Azure block IDs. That is exactly the drift this RFC is meant to avoid.

### 16.7 Keep backends in-tree only

This would give Pocopine more control over adapter quality, but it works
against the main storage goal: teams should be able to plug in their
preferred storage engine. `StorageBackend` stays public, with the shared
contract suite as the quality gate.

## 17. Future work

- Resource-level helpers that attach `ObjectRef` fields to generated CRUD
  resources.
- Rich-text embed integration for `pine-richtext`.
- Image processing and thumbnail generation pipelines.
- Virus scanning hooks.
- Upload telemetry through `pocopine-observe`.
- Object lifecycle jobs: expiration, orphan cleanup, retention policies.
- Static-site-only provider modes for hosts that expose their own anonymous
  upload widgets. These must remain explicit because most upload workflows
  need server authorization.
