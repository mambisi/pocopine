//! `<pine-upload>` - resumable browser file upload primitive.
//!
//! The component stays visually headless like the rest of Pine. It wires a
//! native file input to the `pocopine-storage` browser upload client, mirrors
//! progress into state, exposes cancelation, and emits completion/error events
//! for host applications to refresh their own file views.

use pocopine::prelude::*;
#[cfg(target_arch = "wasm32")]
use pocopine::{current_scope_id, on_scope_unmount_for, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{Event, HtmlInputElement};

#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, collections::HashMap};

#[cfg(target_arch = "wasm32")]
use pocopine_storage::{UploadClient, UploadPhase, UploadProgress};
#[cfg(target_arch = "wasm32")]
use web_sys::{AbortController, File};

#[cfg(target_arch = "wasm32")]
thread_local! {
    static ABORT_CONTROLLERS: RefCell<HashMap<ScopeId, AbortController>> =
        RefCell::new(HashMap::new());
}

/// Detail payload emitted with `pp:upload:complete`.
#[derive(Clone, Debug, Serialize)]
pub struct PineUploadComplete {
    pub file_name: String,
    pub upload_url: String,
    pub bytes_uploaded: u64,
}

/// Detail payload emitted with `pp:upload:error`.
#[derive(Clone, Debug, Serialize)]
pub struct PineUploadError {
    pub file_name: String,
    pub message: String,
}

/// Resumable file upload primitive backed by [`UploadClient`].
#[derive(Serialize, Deserialize)]
#[component(template = "PineUpload.poco", role = "panel")]
pub struct PineUpload {
    /// Server-registered upload scope.
    #[prop]
    pub scope: String,
    /// Native file input `accept` attribute.
    #[prop]
    pub accept: String,
    /// Disables file selection.
    #[prop]
    pub disabled: bool,
    /// Enables browser localStorage upload-url resume for same file metadata.
    #[prop]
    pub auto_resume: bool,
    /// PATCH chunk size in bytes. Zero keeps the upload client default.
    #[prop]
    pub chunk_size: u64,
    pub busy: bool,
    pub canceling: bool,
    pub file_name: String,
    pub status: String,
    pub error: String,
    pub bytes_sent: u64,
    pub bytes_total: u64,
    pub progress: f64,
}

impl Default for PineUpload {
    fn default() -> Self {
        Self {
            scope: String::new(),
            accept: String::new(),
            disabled: false,
            auto_resume: true,
            chunk_size: 0,
            busy: false,
            canceling: false,
            file_name: String::new(),
            status: "Idle".to_string(),
            error: String::new(),
            bytes_sent: 0,
            bytes_total: 0,
            progress: 0.0,
        }
    }
}

#[handlers]
impl PineUpload {
    pub fn on_setup(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(scope_id) = current_scope_id() {
            on_scope_unmount_for(scope_id, move || {
                remove_abort_controller(scope_id);
            });
        }
    }

    pub fn select_file(&mut self, ev: Event) {
        if self.disabled || self.busy {
            return;
        }

        let Some(input) = ev
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        else {
            self.fail("upload input is unavailable");
            return;
        };
        let Some(files) = input.files() else {
            return;
        };
        let Some(file) = files.get(0) else {
            return;
        };

        self.start_upload(file);
        input.set_value("");
    }

    pub fn cancel_upload(&mut self) {
        if !self.busy {
            return;
        }
        self.canceling = true;
        self.status = "Canceling".to_string();

        #[cfg(target_arch = "wasm32")]
        if let Some(scope_id) = current_scope_id() {
            abort_upload(scope_id);
        }
    }
}

impl PineUpload {
    #[cfg(target_arch = "wasm32")]
    fn start_upload(&mut self, file: File) {
        let Some(scope_id) = current_scope_id() else {
            self.fail("upload scope is unavailable");
            return;
        };
        let Some(client) = self.plugins().get::<UploadClient>() else {
            self.fail("upload plugin is not installed");
            return;
        };
        let Ok(controller) = AbortController::new() else {
            self.fail("browser abort controller is unavailable");
            return;
        };

        let file_name = file.name();
        let scope = normalized_scope(&self.scope);
        let auto_resume = self.auto_resume;
        let chunk_size = self.chunk_size;
        let progress_handle = this::<Self>();
        let signal = controller.signal();

        ABORT_CONTROLLERS.with(|controllers| {
            controllers
                .borrow_mut()
                .insert(scope_id, controller.clone());
        });

        self.busy = true;
        self.canceling = false;
        self.file_name = file_name.clone();
        self.status = "Preparing".to_string();
        self.error.clear();
        self.bytes_sent = 0;
        self.bytes_total = file.size() as u64;
        self.progress = 0.0;

        dispatch!(
            async move {
                let mut upload = client
                    .scope(scope)
                    .upload(file)
                    .auto_resume(auto_resume)
                    .abort_signal(signal)
                    .on_progress(move |progress| {
                        progress_handle.update(|state| {
                            state.apply_progress(progress);
                        });
                    });
                if chunk_size > 0 {
                    upload = upload.chunk_size(chunk_size);
                }
                upload.send().await
            }
            .await,
            |s, result| {
                remove_abort_controller(scope_id);
                s.busy = false;
                match result {
                    Ok(upload) => {
                        s.canceling = false;
                        s.error.clear();
                        s.status = "Complete".to_string();
                        s.bytes_sent = upload.bytes_uploaded;
                        s.bytes_total = s.bytes_total.max(upload.bytes_uploaded);
                        s.progress = 100.0;
                        emit_upload_complete(
                            scope_id,
                            PineUploadComplete {
                                file_name: file_name.clone(),
                                upload_url: upload.upload_url,
                                bytes_uploaded: upload.bytes_uploaded,
                            },
                        );
                    }
                    Err(err) => {
                        let message = err.to_string();
                        if s.canceling {
                            s.status = "Canceled".to_string();
                            s.error.clear();
                            s.canceling = false;
                        } else {
                            s.status = "Failed".to_string();
                            s.error = message.clone();
                            emit_upload_error(
                                scope_id,
                                PineUploadError {
                                    file_name: file_name.clone(),
                                    message,
                                },
                            );
                        }
                    }
                }
            },
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start_upload(&mut self, _file: web_sys::File) {
        self.fail("file uploads require the browser runtime");
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.busy = false;
        self.canceling = false;
        self.status = "Failed".to_string();
        self.error = message.into();
    }

    #[cfg(target_arch = "wasm32")]
    fn apply_progress(&mut self, progress: UploadProgress) {
        self.bytes_sent = progress.bytes_sent;
        if let Some(total) = progress.bytes_total {
            self.bytes_total = total;
        }
        let phase = progress.phase;
        self.status = phase_label(&phase).to_string();
        if self.bytes_total > 0 {
            self.progress =
                ((self.bytes_sent as f64 / self.bytes_total as f64) * 100.0).clamp(0.0, 100.0);
        } else if phase == UploadPhase::Complete {
            self.progress = 100.0;
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn normalized_scope(scope: &str) -> String {
    let scope = scope.trim();
    if scope.is_empty() {
        "files".to_string()
    } else {
        scope.to_string()
    }
}

#[cfg(target_arch = "wasm32")]
fn phase_label(phase: &UploadPhase) -> &'static str {
    match phase {
        UploadPhase::Initiating => "Preparing",
        UploadPhase::Uploading => "Uploading",
        UploadPhase::Retrying => "Retrying",
        UploadPhase::Completing => "Completing",
        UploadPhase::Complete => "Complete",
        UploadPhase::Aborted => "Canceled",
        UploadPhase::Failed => "Failed",
        _ => "Uploading",
    }
}

#[cfg(target_arch = "wasm32")]
fn emit_upload_complete(scope_id: ScopeId, detail: PineUploadComplete) {
    if let Some(root) = refs::get_on(scope_id, "root") {
        emit_from(&root, "pp:upload:complete", detail);
    }
}

#[cfg(target_arch = "wasm32")]
fn emit_upload_error(scope_id: ScopeId, detail: PineUploadError) {
    if let Some(root) = refs::get_on(scope_id, "root") {
        emit_from(&root, "pp:upload:error", detail);
    }
}

#[cfg(target_arch = "wasm32")]
fn remove_abort_controller(scope_id: ScopeId) {
    ABORT_CONTROLLERS.with(|controllers| {
        controllers.borrow_mut().remove(&scope_id);
    });
}

#[cfg(target_arch = "wasm32")]
fn abort_upload(scope_id: ScopeId) {
    ABORT_CONTROLLERS.with(|controllers| {
        if let Some(controller) = controllers.borrow().get(&scope_id) {
            controller.abort();
        }
    });
}
