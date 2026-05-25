//! Browser smoke tests for the storage client.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine-storage --test client_browser`

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use js_sys::{Array, Promise};
use pocopine::prelude::*;
use pocopine_storage::{
    storage_plugin, BrowserStorageRequest, BrowserStorageResponse, BrowserStorageTransport,
    ObjectRef, ObjectVisibility, StorageClient, StorageError, StorageResult, TransferPlan,
    UploadPhase, UploadProgress, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Blob};

wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static PLUGIN_RESOLVED: RefCell<bool> = const { RefCell::new(false) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "storage-plugin-probe",
    template_inline = r#"<div class="storage-plugin-probe"></div>"#
)]
struct StoragePluginProbe;

#[handlers]
impl StoragePluginProbe {
    pub fn on_mount(&mut self) {
        let _client = self.plugin::<StorageClient>();
        PLUGIN_RESOLVED.with(|resolved| *resolved.borrow_mut() = true);
    }
}

#[wasm_bindgen_test(async)]
async fn storage_plugin_installs_client_service() {
    PLUGIN_RESOLVED.with(|resolved| *resolved.borrow_mut() = false);
    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<storage-plugin-probe></storage-plugin-probe>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(storage_plugin())
        .register::<StoragePluginProbe>()
        .run();
    settle().await;

    assert!(PLUGIN_RESOLVED.with(|resolved| *resolved.borrow()));
    host.remove();
}

#[wasm_bindgen_test(async)]
async fn upload_blob_sends_initiate_chunks_and_complete_with_progress() {
    pocopine_storage::__reset_browser_transport_for_test();
    let fake = FakeStorageTransport::default();
    let state = fake.state.clone();
    pocopine_storage::__set_browser_transport_for_test(fake);

    let progress = Rc::new(RefCell::new(Vec::<UploadProgress>::new()));
    let progress_for_callback = progress.clone();
    let object = StorageClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .strategy(UploadStrategy::Auto)
        .on_progress(move |event| progress_for_callback.borrow_mut().push(event))
        .send()
        .await
        .unwrap();

    assert_eq!(object.key, "avatars/user-1/photo.txt");
    let state = state.borrow();
    assert_eq!(
        state
            .requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        ["POST", "GET", "PATCH", "PATCH", "PATCH", "POST"]
    );
    assert_eq!(state.patch_offsets, [0, 2, 4]);
    assert_eq!(
        progress
            .borrow()
            .iter()
            .filter(|event| event.phase == UploadPhase::Uploading)
            .map(|event| event.bytes_sent)
            .collect::<Vec<_>>(),
        [0, 2, 2, 4, 4, 5]
    );
    assert_eq!(
        progress.borrow().last().unwrap().phase,
        UploadPhase::Complete
    );
    pocopine_storage::__reset_browser_transport_for_test();
}

#[wasm_bindgen_test(async)]
async fn offset_mismatch_inspects_and_resumes_from_server_offset() {
    pocopine_storage::__reset_browser_transport_for_test();
    let fake = FakeStorageTransport::default();
    fake.state.borrow_mut().mismatch_once = true;
    let state = fake.state.clone();
    pocopine_storage::__set_browser_transport_for_test(fake);

    let object = StorageClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .send()
        .await
        .unwrap();

    assert_eq!(object.size, 5);
    let state = state.borrow();
    assert_eq!(state.patch_offsets, [0, 2, 4]);
    let inspect_count = state
        .requests
        .iter()
        .filter(|request| request.method == "GET")
        .count();
    assert_eq!(
        inspect_count, 2,
        "client should inspect once before upload and once after offset mismatch"
    );
    pocopine_storage::__reset_browser_transport_for_test();
}

#[wasm_bindgen_test(async)]
async fn abort_signal_stops_upload_request() {
    pocopine_storage::__reset_browser_transport_for_test();
    let fake = FakeStorageTransport::default();
    pocopine_storage::__set_browser_transport_for_test(fake);
    let controller = web_sys::AbortController::new().unwrap();
    controller.abort();

    let err = StorageClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .abort_signal(controller.signal())
        .send()
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Client { .. }));
    pocopine_storage::__reset_browser_transport_for_test();
}

#[derive(Clone, Default)]
struct FakeStorageTransport {
    state: Rc<RefCell<FakeState>>,
}

#[derive(Default)]
struct FakeState {
    requests: Vec<SeenRequest>,
    offset: u64,
    patch_offsets: Vec<u64>,
    mismatch_once: bool,
}

#[derive(Clone, Debug)]
struct SeenRequest {
    method: String,
}

impl BrowserStorageTransport for FakeStorageTransport {
    fn request(
        &self,
        request: BrowserStorageRequest,
    ) -> Pin<Box<dyn Future<Output = StorageResult<BrowserStorageResponse>>>> {
        let state = self.state.clone();
        Box::pin(async move {
            if request
                .abort_signal
                .as_ref()
                .is_some_and(|signal| signal.aborted())
            {
                return Err(StorageError::client("request aborted"));
            }

            let mut state = state.borrow_mut();
            state.requests.push(SeenRequest {
                method: request.method.clone(),
            });
            match (request.method.as_str(), request.url.as_str()) {
                ("POST", "/__pocopine/storage/v1/uploads") => {
                    state.offset = 0;
                    Ok(json_response(Ok(session(state.offset))))
                }
                ("GET", "/__pocopine/storage/v1/uploads/session-1") => {
                    Ok(json_response(Ok(session(state.offset))))
                }
                ("PATCH", "/__pocopine/storage/v1/uploads/session-1/bytes") => {
                    let provided = request
                        .headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("Upload-Offset"))
                        .and_then(|(_, value)| value.parse::<u64>().ok())
                        .unwrap();
                    state.patch_offsets.push(provided);
                    if state.mismatch_once {
                        state.mismatch_once = false;
                        state.offset = 2;
                        return Ok(json_response::<UploadSession>(Err(
                            StorageError::offset_mismatch(2, provided),
                        )));
                    }
                    if provided != state.offset {
                        return Ok(json_response::<UploadSession>(Err(
                            StorageError::offset_mismatch(state.offset, provided),
                        )));
                    }
                    let size = request
                        .blob_body
                        .as_ref()
                        .map(|blob| blob.size() as u64)
                        .unwrap_or(0);
                    state.offset += size;
                    Ok(json_response(Ok(session(state.offset))))
                }
                ("POST", "/__pocopine/storage/v1/uploads/session-1/complete") => {
                    Ok(json_response(Ok(object_ref(state.offset))))
                }
                other => Err(StorageError::client(format!(
                    "unexpected storage request: {other:?}"
                ))),
            }
        })
    }
}

fn session(offset: u64) -> UploadSession {
    UploadSession {
        id: UploadSessionId::new("session-1").unwrap(),
        scope: "avatars".to_string(),
        file_name: "photo.txt".to_string(),
        size: Some(5),
        content_type: None,
        strategy: UploadStrategy::Sequential,
        status: UploadSessionStatus::Open,
        next_offset: Some(offset),
        part_size: Some(2),
        plan: TransferPlan {
            min_part_size: None,
            preferred_part_size: Some(2),
            max_part_size: None,
            max_parts: None,
            max_concurrent_parts: 1,
            resumable: true,
        },
        uploaded_parts: Vec::new(),
        expires_at: OffsetDateTime::UNIX_EPOCH + time::Duration::days(1),
    }
}

fn object_ref(size: u64) -> ObjectRef {
    ObjectRef {
        backend: "memory".to_string(),
        scope: "avatars".to_string(),
        key: "avatars/user-1/photo.txt".to_string(),
        version: None,
        etag: None,
        checksum: None,
        content_type: None,
        size,
        visibility: ObjectVisibility::Private,
        metadata: Default::default(),
    }
}

fn json_response<T: Serialize>(result: StorageResult<T>) -> BrowserStorageResponse {
    BrowserStorageResponse {
        status: 200,
        body: serde_json::to_string(&result).unwrap(),
    }
}

fn blob(text: &str) -> Blob {
    let parts = Array::new();
    parts.push(&JsValue::from_str(text));
    Blob::new_with_str_sequence(&parts).unwrap()
}

async fn settle() {
    let _ = JsFuture::from(Promise::resolve(&JsValue::NULL)).await;
}
