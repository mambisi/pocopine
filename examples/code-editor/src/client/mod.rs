use std::cell::RefCell;

use pine_code::{
    CodeChange, CodeError, CodeResult, DocumentRevision, Selection,
    client::{CodeEditorHandle, CodeFinalSnapshot, PineCodeEditor},
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

const SEED: &str = "fn main() {\n    println!(\"Hello, 🦀\");\n}\n";

thread_local! {
    static CHANGES: RefCell<Vec<CodeChange>> = const { RefCell::new(Vec::new()) };
    static FINAL: RefCell<Vec<CodeFinalSnapshot>> = const { RefCell::new(Vec::new()) };
    static HANDLES: RefCell<Vec<CodeEditorHandle>> = const { RefCell::new(Vec::new()) };
}

#[derive(Serialize, Deserialize)]
#[component(template = "CodeDemo.poco", style = "demo.css")]
pub struct CodeDemo {
    pub seed: String,
    pub visible: bool,
    pub read_only: bool,
    pub disabled: bool,
    pub language: String,
    pub status: String,
    pub saved: String,
    pub draft: String,
    pub query: String,
    pub replacement: String,
}

impl Default for CodeDemo {
    fn default() -> Self {
        Self {
            seed: SEED.into(),
            visible: true,
            read_only: false,
            disabled: false,
            language: "rust".into(),
            status: "Ready".into(),
            saved: String::new(),
            draft: String::new(),
            query: String::new(),
            replacement: String::new(),
        }
    }
}

fn source() -> CodeResult<CodeEditorHandle> {
    let host = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector("#source").ok().flatten())
        .ok_or(CodeError::ViewUnavailable)?;
    CodeEditorHandle::from_element(&host)
}

#[handlers]
impl CodeDemo {
    fn editor_ready(&mut self) {
        let Ok(editor) = source() else {
            return;
        };
        HANDLES.with(|handles| handles.borrow_mut().push(editor.clone()));
        let mut subscriptions = Vec::new();
        let parent = this::<Self>();
        if let Ok(subscription) = editor.on_change(move |change| {
            CHANGES.with(|events| events.borrow_mut().push(change.clone()));
            let revision = change.revision.0;
            parent.defer_update(move |demo| demo.status = format!("Revision {revision}"));
        }) {
            subscriptions.push(subscription);
        }
        let parent = this::<Self>();
        if let Ok(subscription) = editor.on_finalize(move |snapshot| {
            FINAL.with(|events| events.borrow_mut().push(snapshot.clone()));
            let snapshot = snapshot.clone();
            parent.defer_update(move |demo| {
                demo.saved = snapshot.committed.text;
                demo.draft = snapshot
                    .interrupted
                    .and_then(|draft| draft.text)
                    .unwrap_or_default();
                demo.status = "Editor closed; committed text retained".into();
            });
        }) {
            subscriptions.push(subscription);
        }
        if let Some(host) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("source"))
        {
            // This owner disappears on each close. Do not accumulate guards on
            // the longer-lived demo scope through repeated editor mounts.
            pocopine::on_before_detach(&host, move || drop(subscriptions));
        }
    }

    fn save(&mut self) {
        match source().and_then(|editor| editor.snapshot()) {
            Ok(snapshot) if snapshot.composing => {
                self.status = "Finish composing before saving.".into()
            }
            Ok(snapshot) if snapshot.text.trim().is_empty() => {
                self.status = "Enter some source before saving.".into()
            }
            Ok(snapshot) => {
                self.saved = snapshot.text;
                self.status = "Saved committed source.".into();
            }
            Err(error) => self.status = error.to_string(),
        }
    }
    fn reset(&mut self) {
        let result =
            source().and_then(|editor| editor.load_document(SEED, editor.snapshot()?.revision));
        self.status = result.map_or_else(
            |error| error.to_string(),
            |_| "Reset document and undo history.".into(),
        );
    }
    fn undo(&mut self) {
        if let Err(error) = source().and_then(|editor| editor.undo()) {
            self.status = error.to_string();
        }
    }
    fn redo(&mut self) {
        if let Err(error) = source().and_then(|editor| editor.redo()) {
            self.status = error.to_string();
        }
    }
    fn indent(&mut self) {
        if let Err(error) = source().and_then(|editor| editor.indent()) {
            self.status = error.to_string();
        }
    }
    fn outdent(&mut self) {
        if let Err(error) = source().and_then(|editor| editor.outdent()) {
            self.status = error.to_string();
        }
    }
    fn toggle_readonly(&mut self) {
        self.read_only = !self.read_only;
    }
    fn toggle_disabled(&mut self) {
        self.disabled = !self.disabled;
    }
    fn toggle_editor(&mut self) {
        self.visible = !self.visible;
    }
    fn find_next(&mut self) {
        let result = source().and_then(|editor| {
            editor.find_next(&self.query, false)?;
            editor.search(&self.query)
        });
        self.status = result.map_or_else(
            |error| error.to_string(),
            |found| format!("{} matches", found.total),
        );
    }
    fn find_previous(&mut self) {
        let result = source().and_then(|editor| {
            editor.find_next(&self.query, true)?;
            editor.search(&self.query)
        });
        self.status = result.map_or_else(
            |error| error.to_string(),
            |found| format!("{} matches", found.total),
        );
    }
    fn replace_current(&mut self) {
        if let Err(error) =
            source().and_then(|editor| editor.replace_current(&self.query, &self.replacement))
        {
            self.status = error.to_string();
        }
    }
    fn replace_all(&mut self) {
        if let Err(error) =
            source().and_then(|editor| editor.replace_all(&self.query, &self.replacement))
        {
            self.status = error.to_string();
        }
    }
    fn recover_draft(&mut self) {
        let result = source()
            .and_then(|editor| editor.load_document(&self.draft, editor.snapshot()?.revision));
        self.status = result.map_or_else(
            |error| error.to_string(),
            |_| "Recovered interrupted draft for review.".into(),
        );
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<PineCodeEditor>()
        .register::<pine::PineInput>()
        .register::<pine::PineButton>()
        .register::<CodeDemo>()
        .run();
}

// Example-only probes let browser tests assert Rust state independently of the
// editable DOM. They are not part of pine-code's production interface.
#[wasm_bindgen]
pub fn inspect_editor() -> String {
    serde_json::to_string(&serde_json::json!({
        "snapshot": source().and_then(|editor| editor.snapshot()),
        "changes": CHANGES.with(|events| events.borrow().clone()),
        "finalized": FINAL.with(|events| events.borrow().clone()),
        "disposed_handles": HANDLES.with(|handles| handles.borrow().iter().filter(|handle| matches!(handle.text(), Err(CodeError::Disposed))).count()),
    }))
    .unwrap()
}

#[wasm_bindgen]
pub fn editor_metrics() -> String {
    serde_json::to_string(&source().and_then(|editor| editor.metrics())).unwrap()
}

#[wasm_bindgen]
pub fn clear_editor_probes() {
    CHANGES.with(|events| events.borrow_mut().clear());
    FINAL.with(|events| events.borrow_mut().clear());
    HANDLES.with(|handles| handles.borrow_mut().clear());
}

#[wasm_bindgen]
pub fn editor_command(
    command: &str,
    value: &str,
    anchor: usize,
    head: usize,
    expected: u64,
) -> String {
    let result = source().and_then(|editor| match command {
        "load" => editor.load_document(value, DocumentRevision(expected)),
        "insert" => editor.insert_text(value),
        "selection" => {
            editor.set_selection(Selection::between(anchor, head), DocumentRevision(expected))
        }
        "undo" => editor.undo(),
        "redo" => editor.redo(),
        "replace-all-x" => editor.replace_all("x", value),
        "find" => editor.find_next(value, false).map(|outcome| {
            outcome.unwrap_or_else(|| {
                let s = editor.snapshot().unwrap();
                pine_code::CommitOutcome {
                    revision: s.revision,
                    view_status: s.view_status,
                }
            })
        }),
        "focus" => {
            editor.focus()?;
            let s = editor.snapshot()?;
            Ok(pine_code::CommitOutcome {
                revision: s.revision,
                view_status: s.view_status,
            })
        }
        "recover" => {
            let view_status = editor.recover_view()?;
            Ok(pine_code::CommitOutcome {
                revision: editor.snapshot()?.revision,
                view_status,
            })
        }
        "language" | "theme" | "tab-behavior" => {
            let mut options = editor.options()?;
            match command {
                "language" => options.language = value.into(),
                "theme" => options.theme = value.into(),
                _ => options.tab_behavior = value.into(),
            }
            editor.configure(options)?;
            let s = editor.snapshot()?;
            Ok(pine_code::CommitOutcome {
                revision: s.revision,
                view_status: s.view_status,
            })
        }
        _ => Err(CodeError::InvalidConfiguration),
    });
    serde_json::to_string(&result).unwrap()
}
