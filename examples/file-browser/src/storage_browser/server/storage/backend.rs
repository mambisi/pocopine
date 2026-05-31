use bytes::Bytes;
use pocopine_storage::{
    CompleteUpload, InitiateUpload, ObjectMetadata, SafeObjectKey, StorageBackend,
    StorageBoxFuture, StorageContext, StorageError, StorageKey, StorageKeyFuture,
    StorageKeyResolver, StorageResult, StorageScope, StorageServer, UploadBody, UploadIntent,
    UploadPolicy, UploadSession, UploadSessionId,
};
use pocopine_storage_gcs::GcsStorageBackend;
use pocopine_storage_s3::S3StorageBackend;

use crate::storage_browser::server::storage::*;

pub(crate) fn storage_server() -> StorageResult<StorageServer> {
    let settings = load_upload_settings()?;
    let policy = UploadPolicy::new(UPLOAD_BACKEND)?
        .max_bytes(MAX_UPLOAD_LIMIT_BYTES)
        .preferred_chunk_size(settings.preferred_chunk_bytes);
    let scope = StorageScope::builder(policy)
        .key_resolver(StorageBrowserUploadKeyResolver)
        .build();

    Ok(StorageServer::builder()
        .backend(UPLOAD_BACKEND, StorageBrowserUploadBackend)?
        .secure_anonymous_cookies(false)
        .scope(UPLOAD_SCOPE, scope)?
        .build())
}

struct StorageBrowserUploadKeyResolver;

impl StorageKeyResolver for StorageBrowserUploadKeyResolver {
    fn resolve_key<'a>(
        &'a self,
        _ctx: &'a StorageContext,
        intent: &'a UploadIntent,
    ) -> StorageKeyFuture<'a> {
        Box::pin(async move {
            let connection_id = upload_metadata(intent, "connection_id")?;
            let mut prefix =
                normalize_prefix(upload_metadata(intent, "prefix").unwrap_or_default());
            if is_internal_storage_key(&prefix) {
                prefix.clear();
            }
            if connection_id.trim().is_empty() {
                return Err(StorageError::policy_rejected(
                    "storage connection is required",
                ));
            }

            let name = sanitize_upload_name(intent.file_name());
            if name.is_empty() {
                return Err(StorageError::policy_rejected("file name is required"));
            }
            let key = SafeObjectKey::parse(format!("{}{}", prefix, name))?;
            let mut metadata = ObjectMetadata::default();
            metadata.insert("original_name", intent.file_name());
            metadata.insert("connection_id", connection_id);
            metadata.insert("prefix", prefix);
            Ok(StorageKey::new(key).metadata(metadata))
        })
    }
}

#[derive(Clone, Debug, Default)]
struct StorageBrowserUploadBackend;

impl StorageBackend for StorageBrowserUploadBackend {
    fn name(&self) -> &'static str {
        UPLOAD_BACKEND
    }

    fn initiate_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        mut request: InitiateUpload,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let settings = load_upload_settings()?;
            if request
                .size
                .is_some_and(|size| size > settings.upload_max_bytes)
            {
                return Err(StorageError::payload_too_large(settings.upload_max_bytes));
            }
            request.policy = upload_policy_for_settings(&settings)?;
            let connection_id = upload_request_metadata(&request, "connection_id")?;
            let backend =
                connection_upload_backend(connection_id, settings.upload_max_bytes).await?;
            backend.initiate_upload(ctx, request).await
        })
    }

    fn inspect_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let (_backend, upload) = upload_backend_for_session(ctx, session).await?;
            Ok(upload)
        })
    }

    fn set_upload_length<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        size: u64,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let (backend, _upload) = upload_backend_for_session(ctx, session.clone()).await?;
            backend.set_upload_length(ctx, session, size).await
        })
    }

    fn append_upload_bytes<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        offset: u64,
        bytes: Bytes,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let (backend, _upload) = upload_backend_for_session(ctx, session.clone()).await?;
            backend
                .append_upload_bytes(ctx, session, offset, bytes)
                .await
        })
    }

    fn upload_part<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        number: u32,
        body: UploadBody,
    ) -> StorageBoxFuture<'a, UploadSession> {
        Box::pin(async move {
            let (backend, _upload) = upload_backend_for_session(ctx, session.clone()).await?;
            backend.upload_part(ctx, session, number, body).await
        })
    }

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, pocopine_storage::ObjectRef> {
        Box::pin(async move {
            let (backend, _upload) =
                upload_backend_for_session(ctx, request.session.clone()).await?;
            backend.complete_upload(ctx, request).await
        })
    }

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()> {
        Box::pin(async move {
            let (backend, _upload) = upload_backend_for_session(ctx, session.clone()).await?;
            backend.abort_upload(ctx, session).await
        })
    }
}

enum ConnectionUploadBackend {
    S3(S3StorageBackend),
    Gcs(GcsStorageBackend),
}

impl ConnectionUploadBackend {
    fn initiate_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: InitiateUpload,
    ) -> StorageBoxFuture<'a, UploadSession> {
        match self {
            Self::S3(backend) => backend.initiate_upload(ctx, request),
            Self::Gcs(backend) => backend.initiate_upload(ctx, request),
        }
    }

    fn inspect_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, UploadSession> {
        match self {
            Self::S3(backend) => backend.inspect_upload(ctx, session),
            Self::Gcs(backend) => backend.inspect_upload(ctx, session),
        }
    }

    fn set_upload_length<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        size: u64,
    ) -> StorageBoxFuture<'a, UploadSession> {
        match self {
            Self::S3(backend) => backend.set_upload_length(ctx, session, size),
            Self::Gcs(backend) => backend.set_upload_length(ctx, session, size),
        }
    }

    fn append_upload_bytes<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        offset: u64,
        bytes: Bytes,
    ) -> StorageBoxFuture<'a, UploadSession> {
        match self {
            Self::S3(backend) => backend.append_upload_bytes(ctx, session, offset, bytes),
            Self::Gcs(backend) => backend.append_upload_bytes(ctx, session, offset, bytes),
        }
    }

    fn upload_part<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
        number: u32,
        body: UploadBody,
    ) -> StorageBoxFuture<'a, UploadSession> {
        match self {
            Self::S3(backend) => backend.upload_part(ctx, session, number, body),
            Self::Gcs(backend) => backend.upload_part(ctx, session, number, body),
        }
    }

    fn complete_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        request: CompleteUpload,
    ) -> StorageBoxFuture<'a, pocopine_storage::ObjectRef> {
        match self {
            Self::S3(backend) => backend.complete_upload(ctx, request),
            Self::Gcs(backend) => backend.complete_upload(ctx, request),
        }
    }

    fn abort_upload<'a>(
        &'a self,
        ctx: &'a StorageContext,
        session: UploadSessionId,
    ) -> StorageBoxFuture<'a, ()> {
        match self {
            Self::S3(backend) => backend.abort_upload(ctx, session),
            Self::Gcs(backend) => backend.abort_upload(ctx, session),
        }
    }
}

async fn connection_upload_backend(
    connection_id: &str,
    max_proxy_upload_bytes: u64,
) -> StorageResult<ConnectionUploadBackend> {
    let config = load_config_storage()?;
    let connection = config
        .connections
        .iter()
        .find(|connection| connection.id() == connection_id)
        .ok_or_else(|| StorageError::policy_rejected("storage connection not found"))?;
    match connection {
        SavedConnection::S3(connection) => connection
            .upload_backend(max_proxy_upload_bytes)
            .map(ConnectionUploadBackend::S3),
        SavedConnection::Gcs(connection) => connection
            .upload_backend(max_proxy_upload_bytes)
            .await
            .map(ConnectionUploadBackend::Gcs),
    }
}

async fn connection_upload_backends() -> StorageResult<Vec<ConnectionUploadBackend>> {
    let config = load_config_storage()?;
    let settings = config
        .settings
        .validate()
        .map_err(|err| StorageError::backend(format!("load storage browser settings: {err}")))?;
    let mut backends = Vec::new();
    for connection in config.connections.iter() {
        let backend = match connection {
            SavedConnection::S3(connection) => {
                ConnectionUploadBackend::S3(connection.upload_backend(settings.upload_max_bytes)?)
            }
            SavedConnection::Gcs(connection) => ConnectionUploadBackend::Gcs(
                connection.upload_backend(settings.upload_max_bytes).await?,
            ),
        };
        backends.push(backend);
    }
    Ok(backends)
}

async fn upload_backend_for_session(
    ctx: &StorageContext,
    session: UploadSessionId,
) -> StorageResult<(ConnectionUploadBackend, UploadSession)> {
    for backend in connection_upload_backends().await? {
        match backend.inspect_upload(ctx, session.clone()).await {
            Ok(upload) => return Ok((backend, upload)),
            Err(StorageError::UnknownUploadSession { .. } | StorageError::Forbidden { .. }) => {}
            Err(err) => return Err(err),
        }
    }
    Err(StorageError::unknown_upload_session(session.to_string()))
}

fn upload_metadata<'a>(intent: &'a UploadIntent, key: &str) -> StorageResult<&'a str> {
    intent.metadata.get(key).map(String::as_str).ok_or_else(|| {
        StorageError::policy_rejected(format!("upload metadata `{key}` is required"))
    })
}

fn upload_request_metadata<'a>(request: &'a InitiateUpload, key: &str) -> StorageResult<&'a str> {
    request
        .metadata
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| {
            StorageError::policy_rejected(format!("upload metadata `{key}` is required"))
        })
}
