// Host-side compile lacks the `web_sys::File` / `UploadClient`
// paths that drive the queue, so several helpers fall back to
// dead code there. They're still part of the wasm execution
// graph, so allow dead_code at the module level rather than
// gating each helper individually.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

//! `<pine-upload-*>` — multi-file resumable upload (compound).
//!
//! Eight parts; entirely visually headless. Behaviour, ARIA and
//! `data-*` state attributes ship; CSS is the author's job.
//!
//! - **Root** (`pine-upload-root`) — owns the queue
//!   (`files: Vec<UploadFile>`), the hidden `<input type="file">`,
//!   validation against `accept` / `max-files` / `max-size-bytes`,
//!   the drag-over signal, and per-file lifecycle through
//!   [`pocopine_storage::UploadClient`]. Emits `pp:upload:complete`
//!   and `pp:upload:error` per file plus `pp:upload:all-complete`
//!   when the queue drains without errors. Provides its Handle.
//! - **Trigger** (`pine-upload-trigger`) — `<button>` that opens
//!   the Root's file picker.
//! - **Dropzone** (`pine-upload-dropzone`) — drag-drop surface that
//!   forwards the dropped FileList to Root. Clickable by default
//!   (set `clickable="false"` to disable).
//! - **Item** (`pine-upload-item`) — one row in the queue. Author
//!   iterates `<template pp-for="file in files" pp-key="file.id">`
//!   and threads each file's fields through as props. Provides its
//!   scope id via `ITEM_SCOPE` so the row's action buttons can
//!   resolve their target file at click time.
//! - **ItemCancel / ItemRetry / ItemRemove** — per-row action
//!   buttons. Inject `ITEM_SCOPE`, read the live `id`, call into
//!   Root.
//! - **Clear** (`pine-upload-clear`) — empties the queue and
//!   cancels any in-flight upload.
//!
//! Root exposes its derived state to the slot as a single
//! scoped-slot object — the author opens with
//! `<template pp-slot="default" pp-let="upload">` and reaches the
//! queue and per-file fields as `upload.files`, `upload.progress`,
//! etc.:
//!
//! ```html
//! <pine-upload-root scope="files" accept="image/*,application/pdf"
//!                   multiple auto-resume chunk-size="1048576"
//!                   @pp:upload:complete="refresh_files">
//!   <template pp-slot="default" pp-let="upload">
//!     <pine-upload-dropzone>
//!       <pine-upload-trigger>Browse</pine-upload-trigger>
//!       <span>or drop files here</span>
//!     </pine-upload-dropzone>
//!
//!     <div pp-show="!upload.files_empty">
//!       <span pp-text="upload.status_label"></span>
//!       <template pp-for="file in upload.files" pp-key="file.id">
//!         <pine-upload-item :id="file.id">
//!           <span pp-text="file.name"></span>
//!           <span pp-text="file.size_label"></span>
//!           <span pp-text="file.progress_label"
//!                 pp-show="file.status == 'uploading'"></span>
//!           <pine-upload-item-cancel pp-show="file.status == 'uploading'">Cancel</pine-upload-item-cancel>
//!           <pine-upload-item-retry  pp-show="file.status == 'error'">Retry</pine-upload-item-retry>
//!           <pine-upload-item-remove>×</pine-upload-item-remove>
//!         </pine-upload-item>
//!       </template>
//!       <pine-upload-clear>Clear all</pine-upload-clear>
//!     </div>
//!   </template>
//! </pine-upload-root>
//! ```

use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id};
#[cfg(target_arch = "wasm32")]
use pocopine::{on_scope_unmount_for, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{Event, HtmlElement, HtmlInputElement};

#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, collections::HashMap};

#[cfg(target_arch = "wasm32")]
use pocopine_storage::{UploadClient, UploadPhase, UploadProgress};
#[cfg(target_arch = "wasm32")]
use web_sys::{AbortController, DragEvent, File, FileList};

create_context!(ROOT: Handle<PineUploadRoot>);
// `ITEM_SCOPE` carries the enclosing Item's scope id so action
// buttons in the row's slot resolve their target file id lazily
// from the Item's live state — same trick as PineTagsInputItem
// (the `:id="file.id"` bind hasn't fired by the time the action
// button's `on_setup` runs).
create_context!(ITEM_SCOPE: ScopeId);

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// Per-Root abort handles, keyed by Root scope id then by file id.
    static ABORT_CONTROLLERS: RefCell<HashMap<ScopeId, HashMap<String, AbortController>>> =
        RefCell::new(HashMap::new());
    /// Per-Root browser File handles, keyed the same way. Held
    /// outside component state because `web_sys::File` is not
    /// serializable.
    static FILES_BY_ID: RefCell<HashMap<ScopeId, HashMap<String, File>>> =
        RefCell::new(HashMap::new());
}

const GENERIC_UPLOAD_ERROR: &str = "Upload failed. Check your connection and try again.";
const UPLOAD_UNAVAILABLE_ERROR: &str = "Upload is not available right now.";

/// One entry in the Root's upload queue. Lifecycle is
/// `queued → uploading → done|error|canceled`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UploadFile {
    /// Stable local id (incrementing counter on Root).
    pub id: String,
    pub name: String,
    pub size: u64,
    pub size_label: String,
    /// Lowercased filename extension (no dot). Empty when none.
    pub extension: String,
    /// Uppercased extension trimmed to ≤3 chars (e.g. `"PDF"`,
    /// `"JPG"`, `"FILE"`). Pre-computed because pine-expr has no
    /// `.toUpperCase()` / `.slice()` for the thumbnail label.
    pub thumb_label: String,
    pub status: String,
    /// 0..=100. 0 while queued / waiting.
    pub progress: f64,
    /// `"47%"`-style label. Pre-computed because pine-expr has no
    /// `Math.round`.
    pub progress_label: String,
    pub bytes_sent: u64,
    pub bytes_total: u64,
    pub error: String,
    /// Server-issued resumable URL once known; for app-level use.
    pub upload_url: String,
}

/// Detail payload emitted with `pp:upload:complete` per file.
#[derive(Clone, Debug, Serialize)]
pub struct PineUploadComplete {
    pub file_id: String,
    pub file_name: String,
    pub upload_url: String,
    pub bytes_uploaded: u64,
}

/// Detail payload emitted with `pp:upload:error` per file.
#[derive(Clone, Debug, Serialize)]
pub struct PineUploadError {
    pub file_id: String,
    pub file_name: String,
    pub message: String,
}

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineUploadRoot.poco", role = "scope", display = "contents")]
#[slot(default, accepts = [
    PineUploadTrigger,
    PineUploadDropzone,
    PineUploadItem,
    PineUploadClear,
])]
pub struct PineUploadRoot {
    /// Server-registered storage scope (matches the host
    /// `StorageScope` name).
    #[prop]
    pub scope: String,
    /// Forwarded to the hidden `<input>` `accept` attribute and
    /// used for local validation. Comma-separated MIME types and
    /// `.ext` extensions; empty disables filtering.
    #[prop]
    pub accept: String,
    /// Allow multi-select on the picker and multi-drop on the
    /// dropzone. Default `true`.
    #[prop]
    pub multiple: bool,
    /// Cap on queued + completed file count. `0` = unlimited.
    #[prop]
    pub max_files: u32,
    /// Per-file byte limit enforced locally. `0` = unlimited (the
    /// server still enforces its `UploadPolicy::max_bytes`).
    #[prop]
    pub max_size_bytes: u64,
    /// Disables all interaction.
    #[prop]
    pub disabled: bool,
    /// Forwarded to the upload client; enables localStorage-based
    /// resume of the same file metadata.
    #[prop]
    pub auto_resume: bool,
    /// PATCH chunk size in bytes. `0` keeps the upload client
    /// default.
    #[prop]
    pub chunk_size: u64,

    /// The queue. Authors iterate with
    /// `<template pp-for="file in files">`.
    pub files: Vec<UploadFile>,
    /// `true` while any file is uploading.
    pub busy: bool,
    /// `idle | dragover | uploading | success | partial | empty`.
    /// Stamped on the Root element as `data-state`.
    pub state: String,
    /// Overall progress across all known files. 0..=100.
    pub progress: f64,
    /// Sum of `bytes_sent` across all queued + uploading + done
    /// files.
    pub bytes_sent: u64,
    /// Sum of `bytes_total` across all queued + uploading + done
    /// files.
    pub bytes_total: u64,
    pub file_count: u32,
    pub done_count: u32,
    pub error_count: u32,
    pub queued_count: u32,
    pub uploading_count: u32,
    /// Human label like `"3 files"` / `"1 file"`.
    pub file_count_label: String,
    /// Human label like `"29.5 MB"`.
    pub total_size_label: String,
    /// Composed status string (`"Uploading 3 of 4 files"`,
    /// `"All files uploaded"`, `"Couldn't upload 1 file"`).
    pub status_label: String,
    pub files_empty: bool,
    pub has_errors: bool,
    pub dragover: bool,
    /// Monotonic id counter (not surfaced to authors).
    pub next_id: u64,
}

impl Default for PineUploadRoot {
    fn default() -> Self {
        Self {
            scope: String::new(),
            accept: String::new(),
            multiple: true,
            max_files: 0,
            max_size_bytes: 0,
            disabled: false,
            auto_resume: true,
            chunk_size: 0,
            files: Vec::new(),
            busy: false,
            state: "empty".to_string(),
            progress: 0.0,
            bytes_sent: 0,
            bytes_total: 0,
            file_count: 0,
            done_count: 0,
            error_count: 0,
            queued_count: 0,
            uploading_count: 0,
            file_count_label: "0 files".to_string(),
            total_size_label: "0 B".to_string(),
            status_label: String::new(),
            files_empty: true,
            has_errors: false,
            dragover: false,
            next_id: 1,
        }
    }
}

#[handlers]
impl PineUploadRoot {
    pub fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());

        #[cfg(target_arch = "wasm32")]
        if let Some(scope_id) = current_scope_id() {
            on_scope_unmount_for(scope_id, move || {
                ABORT_CONTROLLERS.with(|controllers| {
                    if let Some(map) = controllers.borrow_mut().remove(&scope_id) {
                        for controller in map.values() {
                            controller.abort();
                        }
                    }
                });
                FILES_BY_ID.with(|files| {
                    files.borrow_mut().remove(&scope_id);
                });
            });
        }
    }

    /// Triggered by the hidden `<input>`'s `@change`.
    pub fn select_files(&mut self, ev: Event) {
        if self.disabled {
            return;
        }
        let Some(input) = ev
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(files) = input.files() else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        self.ingest_file_list(files);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = files;
        input.set_value("");
    }
}

// Non-handler public API. These live outside `#[handlers]` because
// the macro emits a `FromHandlerArg` dispatch arm for every method
// in its impl, and `FileList` / cfg-gated bodies can't satisfy
// that. Sub-components call these via `root.update(|r| r.foo(..))`.
impl PineUploadRoot {
    /// Called by Dropzone after it pulls the FileList out of a
    /// DragEvent — handlers can't take `DragEvent` directly, so
    /// the Dropzone unpacks it and forwards through here.
    #[cfg(target_arch = "wasm32")]
    pub fn add_files(&mut self, list: FileList) {
        if self.disabled {
            return;
        }
        self.dragover = false;
        self.ingest_file_list(list);
    }

    pub fn on_dragover(&mut self) {
        if self.disabled {
            return;
        }
        if !self.dragover {
            self.dragover = true;
            self.recompute_aggregates();
        }
    }

    pub fn on_dragleave(&mut self) {
        if self.dragover {
            self.dragover = false;
            self.recompute_aggregates();
        }
    }

    /// Open the file picker. Called by the Trigger button and the
    /// Dropzone's click handler.
    pub fn open_picker(&mut self) {
        if !self.disabled {
            #[cfg(target_arch = "wasm32")]
            if let Some(scope_id) = current_scope_id() {
                if let Some(el) = refs::get_on(scope_id, "input") {
                    if let Ok(input) = el.dyn_into::<HtmlInputElement>() {
                        input.click();
                    }
                }
            }
        }
    }

    /// Remove a file from the queue. If the upload is in-flight,
    /// it's aborted first.
    pub fn remove_item(&mut self, id: String) {
        if id.is_empty() {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        abort_one(current_scope_id(), &id);

        if let Some(pos) = self.files.iter().position(|f| f.id == id) {
            self.files.remove(pos);
        }
        self.recompute_aggregates();
        #[cfg(target_arch = "wasm32")]
        forget_file(current_scope_id(), &id);
        // Don't auto-advance the queue after a removal — if the
        // user yanked the active upload, leave their next step to
        // them; nothing else queued can race in the meantime.
    }

    /// Abort an in-flight upload. The upload future will fall
    /// through to the failure branch and stamp the item as
    /// `canceled`.
    pub fn cancel_item(&mut self, id: String) {
        if id.is_empty() {
            return;
        }
        if let Some(file) = self.files.iter().find(|f| f.id == id) {
            if file.status == "uploading" {
                #[cfg(target_arch = "wasm32")]
                abort_one(current_scope_id(), &id);
            }
        }
    }

    /// Reset an errored or canceled item to `queued` and kick the
    /// scheduler.
    pub fn retry_item(&mut self, id: String) {
        if id.is_empty() {
            return;
        }
        let kicked = if let Some(file) = self.files.iter_mut().find(|f| f.id == id) {
            if file.status == "error" || file.status == "canceled" {
                file.status = "queued".to_string();
                file.error.clear();
                file.bytes_sent = 0;
                file.progress = 0.0;
                file.progress_label = "0%".to_string();
                true
            } else {
                false
            }
        } else {
            false
        };
        if kicked {
            self.recompute_aggregates();
            self.kick_next();
        }
    }

    /// Cancel every in-flight upload and drop the queue.
    pub fn clear_all(&mut self) {
        #[cfg(target_arch = "wasm32")]
        abort_all(current_scope_id());
        self.files.clear();
        self.recompute_aggregates();
        #[cfg(target_arch = "wasm32")]
        forget_all(current_scope_id());
    }

    /// Reset every errored item and resume the queue.
    pub fn retry_all(&mut self) {
        let mut kicked = false;
        for file in &mut self.files {
            if file.status == "error" || file.status == "canceled" {
                file.status = "queued".to_string();
                file.error.clear();
                file.bytes_sent = 0;
                file.progress = 0.0;
                file.progress_label = "0%".to_string();
                kicked = true;
            }
        }
        if kicked {
            self.recompute_aggregates();
            self.kick_next();
        }
    }
}

impl PineUploadRoot {
    #[cfg(target_arch = "wasm32")]
    fn ingest_file_list(&mut self, list: FileList) {
        let Some(scope_id) = current_scope_id() else {
            return;
        };
        let count = list.length();
        let take = if self.multiple { count } else { count.min(1) };
        for i in 0..take {
            let Some(file) = list.get(i) else {
                continue;
            };
            self.queue_file(scope_id, file);
        }
        self.recompute_aggregates();
        self.kick_next();
    }

    #[cfg(target_arch = "wasm32")]
    fn queue_file(&mut self, scope_id: ScopeId, file: File) {
        if self.max_files > 0 && self.file_count() >= self.max_files {
            return;
        }
        let name = file.name();
        let size = file.size() as u64;
        let extension = extension_for(&name);
        let thumb_label = thumb_label_for(&extension);
        let id = self.mint_id();
        let mut entry = UploadFile {
            id: id.clone(),
            name: name.clone(),
            size,
            size_label: format_size(size),
            extension,
            thumb_label,
            status: "queued".to_string(),
            progress: 0.0,
            progress_label: "0%".to_string(),
            bytes_sent: 0,
            bytes_total: size,
            error: String::new(),
            upload_url: String::new(),
        };

        if let Err(msg) = self.validate(&name, size) {
            entry.status = "error".to_string();
            entry.error = msg;
            self.files.push(entry);
            return;
        }
        FILES_BY_ID.with(|files| {
            files
                .borrow_mut()
                .entry(scope_id)
                .or_default()
                .insert(id, file);
        });
        self.files.push(entry);
    }

    fn validate(&self, name: &str, size: u64) -> Result<(), String> {
        if self.max_size_bytes > 0 && size > self.max_size_bytes {
            return Err(format!(
                "File exceeds {} limit",
                format_size(self.max_size_bytes)
            ));
        }
        if !self.accept.trim().is_empty() && !accept_allows(&self.accept, name) {
            return Err("File type not supported".to_string());
        }
        Ok(())
    }

    fn mint_id(&mut self) -> String {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        format!("u{id}")
    }

    fn file_count(&self) -> u32 {
        self.files.len() as u32
    }

    #[cfg(target_arch = "wasm32")]
    fn kick_next(&mut self) {
        if self.disabled {
            return;
        }
        if self.files.iter().any(|f| f.status == "uploading") {
            return;
        }
        let Some(next_idx) = self.files.iter().position(|f| f.status == "queued") else {
            return;
        };
        let Some(scope_id) = current_scope_id() else {
            return;
        };
        let id = self.files[next_idx].id.clone();
        let name = self.files[next_idx].name.clone();
        let size = self.files[next_idx].size;
        let Some(file) = FILES_BY_ID.with(|files| {
            files
                .borrow()
                .get(&scope_id)
                .and_then(|map| map.get(&id).cloned())
        }) else {
            // File handle was dropped before its turn — mark
            // errored so the user sees something.
            let entry = &mut self.files[next_idx];
            entry.status = "error".to_string();
            entry.error = "file handle lost".to_string();
            self.recompute_aggregates();
            return;
        };

        self.files[next_idx].status = "uploading".to_string();
        self.files[next_idx].progress = 0.0;
        self.files[next_idx].bytes_sent = 0;
        self.files[next_idx].bytes_total = size;
        self.files[next_idx].error.clear();

        self.start_upload(scope_id, id, name, file);
        self.recompute_aggregates();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn kick_next(&mut self) {}

    #[cfg(target_arch = "wasm32")]
    fn start_upload(&mut self, scope_id: ScopeId, file_id: String, file_name: String, file: File) {
        let Some(client) = self.plugins().get::<UploadClient>() else {
            self.finish_item_failure(&file_id, &file_name, "upload plugin is not installed");
            return;
        };
        let Ok(controller) = AbortController::new() else {
            self.finish_item_failure(
                &file_id,
                &file_name,
                "browser abort controller is unavailable",
            );
            return;
        };
        let scope_name = normalized_scope(&self.scope);
        let auto_resume = self.auto_resume;
        let chunk_size = self.chunk_size;
        let progress_handle = this::<Self>();
        let signal = controller.signal();

        ABORT_CONTROLLERS.with(|controllers| {
            controllers
                .borrow_mut()
                .entry(scope_id)
                .or_default()
                .insert(file_id.clone(), controller.clone());
        });

        let id_for_progress = file_id.clone();
        let id_for_dispatch = file_id.clone();
        let name_for_dispatch = file_name.clone();
        dispatch!(
            async move {
                let mut upload = client
                    .scope(scope_name)
                    .upload(file)
                    .auto_resume(auto_resume)
                    .abort_signal(signal)
                    .on_progress(move |progress| {
                        let id = id_for_progress.clone();
                        progress_handle.update(|state| {
                            state.apply_progress(&id, progress);
                        });
                    });
                if chunk_size > 0 {
                    upload = upload.chunk_size(chunk_size);
                }
                upload.send().await
            }
            .await,
            |s, result| {
                clear_abort_for(scope_id, &id_for_dispatch);
                let canceled = matches!(&result, Err(err) if is_canceled(err));
                match result {
                    Ok(upload) => {
                        s.complete_item(
                            scope_id,
                            &id_for_dispatch,
                            &name_for_dispatch,
                            &upload.upload_url,
                            upload.bytes_uploaded,
                        );
                    }
                    Err(err) => {
                        if canceled {
                            s.cancel_finalize(&id_for_dispatch);
                        } else {
                            s.finish_item_failure(
                                &id_for_dispatch,
                                &name_for_dispatch,
                                &err.to_string(),
                            );
                        }
                    }
                }
                s.recompute_aggregates();
                s.kick_next();
            },
        );
    }

    #[cfg(target_arch = "wasm32")]
    fn complete_item(
        &mut self,
        scope_id: ScopeId,
        file_id: &str,
        file_name: &str,
        upload_url: &str,
        bytes_uploaded: u64,
    ) {
        if let Some(file) = self.files.iter_mut().find(|f| f.id == file_id) {
            file.status = "done".to_string();
            file.progress = 100.0;
            file.progress_label = "100%".to_string();
            file.bytes_sent = bytes_uploaded;
            file.bytes_total = file.bytes_total.max(bytes_uploaded);
            file.upload_url = upload_url.to_string();
            file.error.clear();
        }
        forget_file(Some(scope_id), file_id);
        emit_root_event(
            scope_id,
            "pp:upload:complete",
            PineUploadComplete {
                file_id: file_id.to_string(),
                file_name: file_name.to_string(),
                upload_url: upload_url.to_string(),
                bytes_uploaded,
            },
        );
    }

    fn finish_item_failure(&mut self, file_id: &str, file_name: &str, message: &str) {
        let user_message = user_upload_error_message(message);
        if let Some(file) = self.files.iter_mut().find(|f| f.id == file_id) {
            file.status = "error".to_string();
            file.error = user_message.to_string();
        }
        #[cfg(target_arch = "wasm32")]
        log_upload_error(file_id, file_name, message);
        #[cfg(target_arch = "wasm32")]
        if let Some(scope_id) = current_scope_id() {
            emit_root_event(
                scope_id,
                "pp:upload:error",
                PineUploadError {
                    file_id: file_id.to_string(),
                    file_name: file_name.to_string(),
                    message: user_message.to_string(),
                },
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = file_name;
    }

    fn cancel_finalize(&mut self, file_id: &str) {
        if let Some(file) = self.files.iter_mut().find(|f| f.id == file_id) {
            file.status = "canceled".to_string();
            file.error.clear();
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn apply_progress(&mut self, file_id: &str, progress: UploadProgress) {
        if let Some(file) = self.files.iter_mut().find(|f| f.id == file_id) {
            file.bytes_sent = progress.bytes_sent;
            if let Some(total) = progress.bytes_total {
                file.bytes_total = total;
            }
            let phase = progress.phase;
            file.status = match phase {
                UploadPhase::Complete => "done".to_string(),
                UploadPhase::Aborted => "canceled".to_string(),
                UploadPhase::Failed => "error".to_string(),
                _ => "uploading".to_string(),
            };
            if file.bytes_total > 0 {
                file.progress =
                    ((file.bytes_sent as f64 / file.bytes_total as f64) * 100.0).clamp(0.0, 100.0);
            } else if phase == UploadPhase::Complete {
                file.progress = 100.0;
            }
            file.progress_label = format!("{}%", file.progress.round() as i64);
        }
        self.recompute_aggregates();
    }

    fn recompute_aggregates(&mut self) {
        let mut done = 0u32;
        let mut error = 0u32;
        let mut queued = 0u32;
        let mut uploading = 0u32;
        let mut bytes_sent = 0u64;
        let mut bytes_total = 0u64;
        for file in &self.files {
            bytes_total = bytes_total.saturating_add(file.bytes_total);
            bytes_sent = bytes_sent.saturating_add(file.bytes_sent);
            match file.status.as_str() {
                "done" => done += 1,
                "error" => error += 1,
                "queued" => queued += 1,
                "uploading" => uploading += 1,
                _ => {}
            }
        }
        let count = self.files.len() as u32;
        self.file_count = count;
        self.done_count = done;
        self.error_count = error;
        self.queued_count = queued;
        self.uploading_count = uploading;
        self.bytes_sent = bytes_sent;
        self.bytes_total = bytes_total;
        self.has_errors = error > 0;
        self.busy = uploading > 0 || queued > 0;
        self.files_empty = count == 0;
        self.progress = if bytes_total == 0 {
            0.0
        } else {
            ((bytes_sent as f64 / bytes_total as f64) * 100.0).clamp(0.0, 100.0)
        };
        self.file_count_label = match count {
            0 => "0 files".to_string(),
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        };
        self.total_size_label = format_size(bytes_total);
        self.state = if count == 0 {
            "empty".to_string()
        } else if self.dragover {
            "dragover".to_string()
        } else if uploading > 0 || queued > 0 {
            "uploading".to_string()
        } else if error > 0 && done > 0 {
            "partial".to_string()
        } else if error > 0 {
            "error".to_string()
        } else if done == count {
            "success".to_string()
        } else {
            "idle".to_string()
        };
        self.status_label = compose_status(count, done, error, uploading, queued);

        #[cfg(target_arch = "wasm32")]
        if !self.busy && error == 0 && count > 0 && done == count {
            if let Some(scope_id) = current_scope_id() {
                emit_root_event(scope_id, "pp:upload:all-complete", done);
            }
        }
    }
}

fn compose_status(count: u32, done: u32, error: u32, uploading: u32, queued: u32) -> String {
    if count == 0 {
        return String::new();
    }
    if uploading > 0 || queued > 0 {
        let total = uploading + queued + done + error;
        return format!("Uploading {done} of {total}");
    }
    if error > 0 && done > 0 {
        return format!("Uploaded {done} · {error} failed");
    }
    if error > 0 {
        return if error == 1 {
            "Couldn't upload 1 file".to_string()
        } else {
            format!("Couldn't upload {error} files")
        };
    }
    if done == count {
        return "All files uploaded".to_string();
    }
    String::new()
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Three-char uppercase thumbnail label. Falls back to `"FILE"`
/// when the filename has no extension.
fn thumb_label_for(ext: &str) -> String {
    if ext.is_empty() {
        "FILE".to_string()
    } else {
        ext.chars().take(3).collect::<String>().to_ascii_uppercase()
    }
}

fn extension_for(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// `accept` parser. Accepts comma-separated entries of any of:
/// `.ext`, `text/plain`, `image/*`. Mime matching is bypassed
/// when we don't have an explicit `content_type` on the File (we
/// only see the filename here, so extensions are the reliable
/// signal); wildcard mime entries always pass, deferring to
/// server-side enforcement.
fn accept_allows(accept: &str, name: &str) -> bool {
    let ext = extension_for(name);
    for entry in accept.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" || entry.ends_with("/*") {
            return true;
        }
        if let Some(ext_pat) = entry.strip_prefix('.') {
            if ext_pat.eq_ignore_ascii_case(&ext) {
                return true;
            }
        } else if entry.contains('/') {
            // Plain mime types — we can't verify without a content_type,
            // so accept the entry optimistically and let the server reject.
            return true;
        }
    }
    false
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
fn is_canceled(err: &impl ToString) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("abort") || msg.contains("cancel")
}

fn user_upload_error_message(message: &str) -> &'static str {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("upload plugin is not installed")
        || normalized.contains("abort controller")
        || normalized.contains("file handle lost")
    {
        UPLOAD_UNAVAILABLE_ERROR
    } else {
        GENERIC_UPLOAD_ERROR
    }
}

#[cfg(target_arch = "wasm32")]
fn log_upload_error(file_id: &str, file_name: &str, message: &str) {
    let message = format!("pine-upload failed for {file_name} ({file_id}): {message}");
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&message));
}

#[cfg(target_arch = "wasm32")]
fn abort_one(scope_id: Option<ScopeId>, file_id: &str) {
    let Some(scope_id) = scope_id else { return };
    ABORT_CONTROLLERS.with(|controllers| {
        if let Some(map) = controllers.borrow().get(&scope_id) {
            if let Some(controller) = map.get(file_id) {
                controller.abort();
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn abort_all(scope_id: Option<ScopeId>) {
    let Some(scope_id) = scope_id else { return };
    ABORT_CONTROLLERS.with(|controllers| {
        if let Some(map) = controllers.borrow().get(&scope_id) {
            for controller in map.values() {
                controller.abort();
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn clear_abort_for(scope_id: ScopeId, file_id: &str) {
    ABORT_CONTROLLERS.with(|controllers| {
        if let Some(map) = controllers.borrow_mut().get_mut(&scope_id) {
            map.remove(file_id);
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn forget_file(scope_id: Option<ScopeId>, file_id: &str) {
    let Some(scope_id) = scope_id else { return };
    FILES_BY_ID.with(|files| {
        if let Some(map) = files.borrow_mut().get_mut(&scope_id) {
            map.remove(file_id);
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn forget_all(scope_id: Option<ScopeId>) {
    let Some(scope_id) = scope_id else { return };
    FILES_BY_ID.with(|files| {
        files.borrow_mut().remove(&scope_id);
    });
}

#[cfg(target_arch = "wasm32")]
fn emit_root_event<D: Serialize>(scope_id: ScopeId, name: &str, detail: D) {
    if let Some(root) = refs::get_on(scope_id, "root") {
        emit_from(&root, name, detail);
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineUploadTrigger.poco", role = "interactive")]
pub struct PineUploadTrigger {
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub busy: bool,
}

#[handlers]
impl PineUploadTrigger {
    pub fn click(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.open_picker());
        }
    }
}

// ── Dropzone ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineUploadDropzone.poco", role = "panel")]
pub struct PineUploadDropzone {
    /// Open the picker when the surface is clicked. Default `true`.
    #[prop]
    pub clickable: bool,
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub state: String,
    #[observe(ROOT)]
    pub dragover: bool,
    #[observe(ROOT)]
    pub busy: bool,
    #[observe(ROOT)]
    pub files_empty: bool,
}

impl Default for PineUploadDropzone {
    fn default() -> Self {
        Self {
            clickable: true,
            disabled: false,
            state: "empty".to_string(),
            dragover: false,
            busy: false,
            files_empty: true,
        }
    }
}

#[handlers]
impl PineUploadDropzone {
    pub fn on_click(&mut self, ev: Event) {
        if self.disabled || !self.clickable {
            return;
        }
        // Skip click-through coming from a nested Trigger button —
        // it opens the picker on its own and would otherwise fire
        // twice.
        if let Some(target) = ev.target().and_then(|t| t.dyn_into::<HtmlElement>().ok()) {
            if target
                .closest("pine-upload-trigger,button,a,input")
                .ok()
                .flatten()
                .is_some()
            {
                return;
            }
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.open_picker());
        }
    }

    pub fn on_dragenter(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.on_dragover());
        }
    }

    pub fn on_dragover_event(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.on_dragover());
        }
    }

    pub fn on_dragleave(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.on_dragleave());
        }
    }

    pub fn on_drop(&mut self, _ev: Event) {
        #[cfg(target_arch = "wasm32")]
        if let Ok(drag) = _ev.dyn_into::<DragEvent>() {
            if let Some(transfer) = drag.data_transfer() {
                if let Some(list) = transfer.files() {
                    if let Some(root) = ROOT.inject() {
                        root.update(|r: &mut PineUploadRoot| r.add_files(list));
                        return;
                    }
                }
            }
        }
        // Even when the drop carried no files, clear the dragover
        // flag so the surface doesn't get stuck looking active.
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.on_dragleave());
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineUploadItem.poco", role = "panel")]
#[slot(default)]
pub struct PineUploadItem {
    /// Stable file id, threaded through from
    /// `<template pp-for="file in files" pp-key="file.id"
    /// :id="file.id">`.
    #[prop]
    pub id: String,
    #[prop]
    pub name: String,
    #[prop]
    pub size_label: String,
    #[prop]
    pub extension: String,
    #[prop]
    pub status: String,
    #[prop]
    pub progress: f64,
    #[prop]
    pub error: String,
}

#[handlers]
impl PineUploadItem {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            ITEM_SCOPE.provide(scope);
        }
    }
}

// ── Item action buttons ───────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineUploadItemCancel.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineUploadItemCancel {
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineUploadItemCancel {
    pub fn click(&mut self) {
        if self.disabled {
            return;
        }
        let Some(id) = resolve_item_id() else {
            return;
        };
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.cancel_item(id));
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineUploadItemRetry.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineUploadItemRetry {
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineUploadItemRetry {
    pub fn click(&mut self) {
        if self.disabled {
            return;
        }
        let Some(id) = resolve_item_id() else {
            return;
        };
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.retry_item(id));
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineUploadItemRemove.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineUploadItemRemove {
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineUploadItemRemove {
    pub fn click(&mut self) {
        if self.disabled {
            return;
        }
        let Some(id) = resolve_item_id() else {
            return;
        };
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.remove_item(id));
        }
    }
}

fn resolve_item_id() -> Option<String> {
    let item_scope = ITEM_SCOPE.inject()?;
    #[cfg(target_arch = "wasm32")]
    {
        let scope = Scope::find(item_scope)?;
        let id = scope
            .state
            .borrow()
            .get("id")
            .as_string()
            .unwrap_or_default();
        if id.is_empty() {
            None
        } else {
            Some(id)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = item_scope;
        None
    }
}

// ── Clear ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineUploadClear.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineUploadClear {
    #[observe(ROOT)]
    pub disabled: bool,
    #[observe(ROOT)]
    pub files_empty: bool,
}

#[handlers]
impl PineUploadClear {
    pub fn click(&mut self) {
        if self.disabled || self.files_empty {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineUploadRoot| r.clear_all());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{user_upload_error_message, GENERIC_UPLOAD_ERROR, UPLOAD_UNAVAILABLE_ERROR};

    #[test]
    fn storage_errors_are_hidden_from_user_message() {
        assert_eq!(
            user_upload_error_message("storage client error: fetch failed: JsValue(TypeError)"),
            GENERIC_UPLOAD_ERROR
        );
    }

    #[test]
    fn internal_setup_errors_get_short_unavailable_message() {
        assert_eq!(
            user_upload_error_message("upload plugin is not installed"),
            UPLOAD_UNAVAILABLE_ERROR
        );
    }
}
