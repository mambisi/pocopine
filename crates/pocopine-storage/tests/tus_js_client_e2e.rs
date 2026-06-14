#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::process::Command;

use pocopine_server::axum::{self, Router};
use pocopine_server::tokio;
use pocopine_storage::{
    MemoryStorageBackend, SafeObjectKey, StorageContext, StorageKey, StorageKeyFuture,
    StorageKeyResolver, StorageResult, StorageScope, StorageServer, UploadIntent, UploadPolicy,
    storage_tus_server_plugin,
};

const EXPECTED_OBJECT_KEY: &str = "avatars/tus-js/photo.txt";
const EXPECTED_OBJECT_BYTES: &[u8] = b"hello from tus-js-client";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node.js and npm-installed tus-js-client"]
async fn tus_js_client_uploads_to_storage_tus_mount() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = workspace_root();
    let tus_package = workspace.join("node_modules/tus-js-client/package.json");
    if !tus_package.exists() {
        return Err("run `npm install` before `npm run test:storage-tus-e2e`".into());
    }

    let backend = MemoryStorageBackend::new();
    let storage = storage_server(backend.clone())?;
    let app = finalize_tus(storage);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("tus_js_client_e2e.mjs");
    let output = Command::new("node")
        .arg(script)
        .arg(format!("http://{addr}"))
        .current_dir(workspace)
        .output();
    server.abort();
    let output = output?;

    assert!(
        output.status.success(),
        "tus-js-client e2e failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        backend.object_bytes(EXPECTED_OBJECT_KEY)?,
        Some(EXPECTED_OBJECT_BYTES.to_vec())
    );

    Ok(())
}

fn storage_server(backend: MemoryStorageBackend) -> StorageResult<StorageServer> {
    let policy = UploadPolicy::new("memory")?
        .max_bytes(1024)
        .allowed_content_types(["text/plain"])
        .allowed_extensions(["txt"])
        .preferred_chunk_size(5);
    let scope = StorageScope::builder(policy)
        .key_resolver(FixedKeyResolver)
        .build();

    Ok(StorageServer::builder()
        .backend("memory", backend)?
        .scope("avatars", scope)?
        .build())
}

fn finalize_tus(storage: StorageServer) -> Router {
    pocopine_server::__reset_for_test();
    pocopine_server::Server::new(Router::new())
        .plugin(storage_tus_server_plugin(storage))
        .try_finalize()
        .unwrap()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("storage crate should live under <workspace>/crates/pocopine-storage")
        .to_path_buf()
}

#[derive(Clone, Debug)]
struct FixedKeyResolver;

impl StorageKeyResolver for FixedKeyResolver {
    fn resolve_key<'a>(
        &'a self,
        _ctx: &'a StorageContext,
        _intent: &'a UploadIntent,
    ) -> StorageKeyFuture<'a> {
        Box::pin(async { Ok(StorageKey::new(SafeObjectKey::parse(EXPECTED_OBJECT_KEY)?)) })
    }
}
