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
    storage_plugin, upload_plugin, BrowserStorageRequest, BrowserStorageResponse,
    BrowserStorageTransport, ObjectRef, ObjectVisibility, StorageClient, StorageError,
    StorageResponse, StorageResult, TransferPlan, UploadClient, UploadPhase, UploadProgress,
    UploadSession, UploadSessionId, UploadSessionStatus, UploadStrategy,
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
    static UPLOAD_PLUGIN_RESOLVED: RefCell<bool> = const { RefCell::new(false) };
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

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "upload-plugin-probe",
    template_inline = r#"<div class="upload-plugin-probe"></div>"#
)]
struct UploadPluginProbe;

#[handlers]
impl UploadPluginProbe {
    pub fn on_mount(&mut self) {
        let _client = self.plugin::<UploadClient>();
        UPLOAD_PLUGIN_RESOLVED.with(|resolved| *resolved.borrow_mut() = true);
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
async fn upload_plugin_installs_client_service() {
    UPLOAD_PLUGIN_RESOLVED.with(|resolved| *resolved.borrow_mut() = false);
    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<upload-plugin-probe></upload-plugin-probe>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(upload_plugin())
        .register::<UploadPluginProbe>()
        .run();
    settle().await;

    assert!(UPLOAD_PLUGIN_RESOLVED.with(|resolved| *resolved.borrow()));
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
async fn transient_client_error_retries_with_backoff() {
    pocopine_storage::__reset_browser_transport_for_test();
    let fake = FakeStorageTransport::default();
    fake.state.borrow_mut().client_error_once = true;
    let state = fake.state.clone();
    pocopine_storage::__set_browser_transport_for_test(fake);

    let object = StorageClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .retry_limit(1)
        .retry_base_delay_ms(0)
        .send()
        .await
        .unwrap();

    assert_eq!(object.size, 5);
    assert_eq!(state.borrow().patch_offsets, [0, 0, 2, 4]);
    pocopine_storage::__reset_browser_transport_for_test();
}

#[wasm_bindgen_test(async)]
async fn local_storage_resume_is_opt_in() {
    pocopine_storage::__reset_browser_transport_for_test();
    window()
        .unwrap()
        .local_storage()
        .unwrap()
        .unwrap()
        .set_item(
            "pocopine.storage.resume.v1:avatars:photo.txt:5:blob",
            &serde_json::to_string(&session(2)).unwrap(),
        )
        .unwrap();
    let fake = FakeStorageTransport::default();
    let state = fake.state.clone();
    pocopine_storage::__set_browser_transport_for_test(fake);

    StorageClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .send()
        .await
        .unwrap();

    assert_eq!(state.borrow().requests.first().unwrap().method, "POST");
    window()
        .unwrap()
        .local_storage()
        .unwrap()
        .unwrap()
        .remove_item("pocopine.storage.resume.v1:avatars:photo.txt:5:blob")
        .unwrap();
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

#[wasm_bindgen_test(async)]
async fn upload_client_sends_create_and_patch_requests() {
    pocopine_storage::__reset_browser_transport_for_test();
    let fake = FakeStorageTransport::default();
    let state = fake.state.clone();
    pocopine_storage::__set_browser_transport_for_test(fake);

    let progress = Rc::new(RefCell::new(Vec::<UploadProgress>::new()));
    let progress_for_callback = progress.clone();
    let result = UploadClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .metadata("filetype", "text/plain")
        .chunk_size(2)
        .on_progress(move |event| progress_for_callback.borrow_mut().push(event))
        .send()
        .await
        .unwrap();

    assert_eq!(
        result.upload_url,
        "/__pocopine/storage/tus/v1/avatars/uploads/tus-session-1"
    );
    assert_eq!(result.bytes_uploaded, 5);
    let state = state.borrow();
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|request| request.url.starts_with("/__pocopine/storage/tus/v1"))
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        ["POST", "PATCH", "PATCH", "PATCH"]
    );
    assert_eq!(state.tus_patch_offsets, [0, 2, 4]);
    let create = state
        .requests
        .iter()
        .find(|request| request.method == "POST" && request.url.contains("/tus/v1/"))
        .unwrap();
    assert_eq!(header(create, "Tus-Resumable"), Some("1.0.0"));
    assert_eq!(header(create, "Upload-Length"), Some("5"));
    assert_eq!(
        header(create, "Upload-Metadata"),
        Some("filename cGhvdG8udHh0,filetype dGV4dC9wbGFpbg==")
    );
    assert_eq!(
        progress.borrow().last().unwrap().phase,
        UploadPhase::Complete
    );
    pocopine_storage::__reset_browser_transport_for_test();
}

#[wasm_bindgen_test(async)]
async fn upload_client_heads_and_resumes_on_offset_conflict() {
    pocopine_storage::__reset_browser_transport_for_test();
    let fake = FakeStorageTransport::default();
    fake.state.borrow_mut().tus_conflict_once = true;
    let state = fake.state.clone();
    pocopine_storage::__set_browser_transport_for_test(fake);

    let result = UploadClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .chunk_size(2)
        .send()
        .await
        .unwrap();

    assert_eq!(result.bytes_uploaded, 5);
    let state = state.borrow();
    assert_eq!(state.tus_patch_offsets, [0, 2, 4]);
    let head_count = state
        .requests
        .iter()
        .filter(|request| request.method == "HEAD" && request.url.contains("/tus/v1/"))
        .count();
    assert_eq!(head_count, 1);
    pocopine_storage::__reset_browser_transport_for_test();
}

#[wasm_bindgen_test(async)]
async fn upload_client_local_storage_resume_is_opt_in() {
    pocopine_storage::__reset_browser_transport_for_test();
    let key = "pocopine.storage.upload.resume.v1:avatars:photo.txt:5:blob";
    window()
        .unwrap()
        .local_storage()
        .unwrap()
        .unwrap()
        .set_item(
            key,
            "/__pocopine/storage/tus/v1/avatars/uploads/tus-session-1",
        )
        .unwrap();
    let fake = FakeStorageTransport::default();
    fake.state.borrow_mut().tus_offset = 2;
    let state = fake.state.clone();
    pocopine_storage::__set_browser_transport_for_test(fake);

    UploadClient::new()
        .scope("avatars")
        .upload_blob(blob("hello"), "photo.txt")
        .chunk_size(2)
        .auto_resume(true)
        .send()
        .await
        .unwrap();

    let state = state.borrow();
    let tus_methods = state
        .requests
        .iter()
        .filter(|request| request.url.starts_with("/__pocopine/storage/tus/v1"))
        .map(|request| request.method.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tus_methods, ["HEAD", "PATCH", "PATCH"]);
    assert_eq!(state.tus_patch_offsets, [2, 4]);
    assert!(window()
        .unwrap()
        .local_storage()
        .unwrap()
        .unwrap()
        .get_item(key)
        .unwrap()
        .is_none());
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
    client_error_once: bool,
    tus_offset: u64,
    tus_patch_offsets: Vec<u64>,
    tus_conflict_once: bool,
}

#[derive(Clone, Debug)]
struct SeenRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
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
                url: request.url.clone(),
                headers: request.headers.clone(),
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
                    if state.client_error_once {
                        state.client_error_once = false;
                        return Err(StorageError::client("temporary transport failure"));
                    }
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
                ("POST", "/__pocopine/storage/tus/v1/avatars/uploads") => {
                    state.tus_offset = 0;
                    Ok(tus_response(
                        201,
                        [
                            (
                                "location",
                                "/__pocopine/storage/tus/v1/avatars/uploads/tus-session-1",
                            ),
                            ("upload-offset", "0"),
                            ("tus-resumable", "1.0.0"),
                        ],
                        "",
                    ))
                }
                ("HEAD", "/__pocopine/storage/tus/v1/avatars/uploads/tus-session-1") => {
                    Ok(tus_response(
                        204,
                        vec![
                            ("upload-offset".to_string(), state.tus_offset.to_string()),
                            ("upload-length".to_string(), "5".to_string()),
                            ("tus-resumable".to_string(), "1.0.0".to_string()),
                        ],
                        "",
                    ))
                }
                ("PATCH", "/__pocopine/storage/tus/v1/avatars/uploads/tus-session-1") => {
                    let provided = request
                        .headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("Upload-Offset"))
                        .and_then(|(_, value)| value.parse::<u64>().ok())
                        .unwrap();
                    state.tus_patch_offsets.push(provided);
                    if state.tus_conflict_once {
                        state.tus_conflict_once = false;
                        state.tus_offset = 2;
                        return Ok(tus_response(409, [("tus-resumable", "1.0.0")], "conflict"));
                    }
                    if provided != state.tus_offset {
                        return Ok(tus_response(409, [("tus-resumable", "1.0.0")], "conflict"));
                    }
                    let size = request
                        .blob_body
                        .as_ref()
                        .map(|blob| blob.size() as u64)
                        .unwrap_or(0);
                    state.tus_offset += size;
                    Ok(tus_response(
                        204,
                        vec![
                            ("upload-offset".to_string(), state.tus_offset.to_string()),
                            ("tus-resumable".to_string(), "1.0.0".to_string()),
                        ],
                        "",
                    ))
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
        metadata: Default::default(),
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
    let status = match &result {
        Ok(_) => 200,
        Err(StorageError::OffsetMismatch { .. }) => 409,
        Err(StorageError::Unauthorized { .. }) => 401,
        Err(StorageError::Forbidden { .. }) => 403,
        Err(StorageError::UnknownUploadSession { .. }) => 404,
        Err(StorageError::PayloadTooLarge { .. }) => 413,
        Err(StorageError::PolicyRejected { .. }) => 422,
        Err(_) => 500,
    };
    BrowserStorageResponse {
        status,
        headers: Vec::new(),
        body: serde_json::to_string(&StorageResponse::from_result(result)).unwrap(),
    }
}

fn tus_response<K, V, I>(status: u16, headers: I, body: &str) -> BrowserStorageResponse
where
    K: Into<String>,
    V: Into<String>,
    I: IntoIterator<Item = (K, V)>,
{
    BrowserStorageResponse {
        status,
        headers: headers
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect(),
        body: body.to_string(),
    }
}

fn header<'a>(request: &'a SeenRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn blob(text: &str) -> Blob {
    let parts = Array::new();
    parts.push(&JsValue::from_str(text));
    Blob::new_with_str_sequence(&parts).unwrap()
}

async fn settle() {
    let _ = JsFuture::from(Promise::resolve(&JsValue::NULL)).await;
}
