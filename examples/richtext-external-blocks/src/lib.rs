//! Interactive reference surface for RFC-113 typed external rich-text blocks.
//!
//! The document persists semantic nodes only. Pocopine components are paired
//! through `RuntimeBuilder::with_view`, native DOM comes from `NodeDomSpec`,
//! and every author-owned shell declares its content boundary at compile time.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use pine_richtext::RichTextNodeType;
use pine_richtext::extensions::{TaskItemAttrs, TaskItemNode, TaskListExtension};
use pine_richtext::model::{Attrs, Fragment, Node, Schema};
use pine_richtext::runtime::{self, EditorRuntime, RuntimeBuilder};
use pine_richtext::state::{EditorState, EditorStateConfig, Selection};
use pine_richtext::view::root::CommandRequest;
use pine_richtext::view::{
    Doc, DocChangeSubscription, Editor as RichTextEditor, NodeViewError, NodeViewHandle,
    NodeViewUpdate, PineRichTextRoot, RichTextNodeView, SelectionChangeSubscription,
    SelectionSnapshot,
};
use pine_richtext_extensions::bubble_menu::{BubbleMenuController, BubbleMenuOptions};
use pine_richtext_extensions::tables::{
    TableAlignment, TableAttrs, TableCellAttrs, TableCellNode, TableHeaderCellAttrs,
    TableHeaderCellNode, TableNode, TableRowAttrs, TableRowNode, TablesExtension,
};
use pine_richtext_extensions::tags::{TagAttrs, TagKind, TagNode, TagsExtension};
use pocopine::prelude::*;
use pocopine::{Refs, current_scope_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;
use web_sys::{HtmlInputElement, InputEvent};

const RUNTIME_NAME: &str = "external-blocks";

thread_local! {
    static DEMO_RESOURCES: RefCell<HashMap<ScopeId, DemoResources>> =
        RefCell::new(HashMap::new());
    static DEMO_HANDLE: RefCell<Option<Handle<ExternalBlocksDemo>>> =
        const { RefCell::new(None) };
    static TASK_LIFECYCLE: RefCell<TaskLifecycle> =
        const { RefCell::new(TaskLifecycle::new()) };
}

struct DemoResources {
    bubble: BubbleMenuController,
    _document: DocChangeSubscription,
    _selection: SelectionChangeSubscription,
}

#[derive(Clone, Copy, Debug, Default)]
struct TaskLifecycle {
    mounts: u64,
    syncs: u64,
    unmounts: u64,
    active: u64,
}

impl TaskLifecycle {
    const fn new() -> Self {
        Self {
            mounts: 0,
            syncs: 0,
            unmounts: 0,
            active: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchOption {
    pub key: String,
    pub label: String,
    pub hint: String,
    pub action: String,
}

#[derive(Serialize, Deserialize)]
#[component(
    template = "ExternalBlocksDemo.poco",
    style = "app.css",
    role = "panel",
    display = "block"
)]
pub struct ExternalBlocksDemo {
    pub initial_state: Value,
    pub semantic_json: String,
    pub markdown: String,
    pub code_tab: String,
    pub status: String,
    pub error: String,
    pub selection_status: String,
    pub bubble_status: String,
    pub bubble_anchor: String,
    pub bubble_key: String,
    pub search_query: String,
    pub search_status: String,
    pub search_results: Vec<SearchOption>,
    pub task_mounts: u64,
    pub task_syncs: u64,
    pub task_unmounts: u64,
    pub task_active: u64,
}

impl Default for ExternalBlocksDemo {
    fn default() -> Self {
        Self {
            initial_state: Value::Null,
            semantic_json: String::new(),
            markdown: String::new(),
            code_tab: "markdown".into(),
            status: "Preparing the typed runtime…".into(),
            error: String::new(),
            selection_status: "Place a caret or select content to inspect it.".into(),
            bubble_status: "closed".into(),
            bubble_anchor: "none".into(),
            bubble_key: String::new(),
            search_query: String::new(),
            search_status: "Select text, then search from the floating menu.".into(),
            search_results: command_options(""),
            task_mounts: 0,
            task_syncs: 0,
            task_unmounts: 0,
            task_active: 0,
        }
    }
}

#[handlers]
impl ExternalBlocksDemo {
    fn on_setup(&mut self) {
        match seed_payload(true) {
            Ok(payload) => {
                self.initial_state = payload.state;
                self.semantic_json = payload.json;
                self.markdown = payload.markdown;
                self.status = "Typed document ready — interact with any block below.".into();
            }
            Err(error) => {
                self.error = error;
                self.status = "The typed seed document could not be created.".into();
            }
        }
        self.apply_lifecycle(task_lifecycle());
    }

    fn on_ready(&self, refs: Refs, scope: ScopeId, handle: Handle<Self>) {
        DEMO_HANDLE.with(|slot| *slot.borrow_mut() = Some(handle.clone()));

        let Some(root) = refs.get("root") else {
            handle.defer_update(|state| state.error = "demo root ref is unavailable".into());
            return;
        };
        let Some(menu) = refs.get("bubble_menu") else {
            handle.defer_update(|state| state.error = "bubble-menu ref is unavailable".into());
            return;
        };
        let Some(editor) = RichTextEditor::find(&root) else {
            handle.defer_update(|state| state.error = "rich-text editor is unavailable".into());
            return;
        };

        let mut options = BubbleMenuOptions::default();
        options.searchable = true;
        options.offset = 10.0;
        options.viewport_padding = 16.0;
        options.debounce_ms = 8.0;
        let options = options.with_should_show(|context| {
            !context
                .snapshot
                .enclosing_block_types
                .iter()
                .any(|name| name == "code_block")
        });

        let bubble_handle = handle.clone();
        let bubble = match BubbleMenuController::attach_with_state(
            editor.clone(),
            menu,
            options,
            move |next| {
                bubble_handle.defer_update(move |state| {
                    state.bubble_status = if next.visible { "open" } else { "closed" }.into();
                    state.bubble_anchor = format!("{:?}", next.anchor_kind).to_ascii_lowercase();
                });
            },
        ) {
            Ok(controller) => controller,
            Err(error) => {
                handle.defer_update(move |state| state.error = error.to_string());
                return;
            }
        };

        let bubble_key = bubble.key().to_string();
        let runtime = runtime::registry::resolve(Some(RUNTIME_NAME));
        let document_handle = handle.clone();
        let document_runtime = runtime.clone();
        let document = editor.on_update::<Doc, _>(move |doc| {
            let (semantic_json, markdown, error) = outputs_for_value(&doc, &document_runtime);
            document_handle.defer_update(move |state| {
                state.semantic_json = semantic_json;
                state.markdown = markdown;
                state.error = error;
                state.status = "Committed transaction reflected in both semantic outputs.".into();
            });
        });

        let selection_handle = handle.clone();
        let selection = editor.on_selection_change(move |snapshot| {
            let summary = summarize_selection(&snapshot);
            selection_handle.defer_update(move |state| state.selection_status = summary);
        });

        DEMO_RESOURCES.with(|resources| {
            resources.borrow_mut().insert(
                scope,
                DemoResources {
                    bubble,
                    _document: document,
                    _selection: selection,
                },
            );
        });

        let counts = task_lifecycle();
        handle.defer_update(move |state| {
            state.bubble_key = bubble_key;
            state.apply_lifecycle(counts);
            state.status = "Editor, typed views, and selection observers are live.".into();
        });
    }

    fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            DEMO_RESOURCES.with(|resources| {
                resources.borrow_mut().remove(&scope);
            });
        }
        DEMO_HANDLE.with(|slot| slot.borrow_mut().take());
    }

    pub fn toggle_bold(&mut self) {
        self.dispatch(
            CommandRequest::ToggleMark {
                mark: "strong".into(),
            },
            "Toggled bold",
        );
    }

    pub fn toggle_italic(&mut self) {
        self.dispatch(
            CommandRequest::ToggleMark { mark: "em".into() },
            "Toggled italic",
        );
    }

    pub fn insert_info_tag(&mut self) {
        self.insert_tag("typed-api", "Typed API", TagKind::Info);
    }

    pub fn insert_success_tag(&mut self) {
        self.insert_tag("verified", "Verified", TagKind::Success);
    }

    pub fn select_tag(&mut self) {
        self.custom(
            "select_tag",
            json!({}),
            "Selected the adjacent tag as one semantic atom",
        );
    }

    pub fn delete_tag(&mut self) {
        self.custom(
            "delete_tag",
            json!({}),
            "Deleted the selected or adjacent tag atom",
        );
    }

    pub fn insert_table(&mut self) {
        self.custom(
            "insert_table",
            json!({ "rows": 3, "columns": 3 }),
            "Inserted a typed 3 × 3 table",
        );
    }

    pub fn widen_column(&mut self) {
        self.custom(
            "set_column_width",
            json!({ "column": 0, "width": 260 }),
            "Committed a 260 px width for the first semantic column",
        );
    }

    pub fn heighten_row(&mut self) {
        self.custom(
            "set_row_height",
            json!({ "row": 0, "height": 58 }),
            "Committed a 58 px height for the first semantic row",
        );
    }

    pub fn undo(&mut self) {
        self.run_editor("Undo", RichTextEditor::undo);
    }

    pub fn redo(&mut self) {
        self.run_editor("Redo", RichTextEditor::redo);
    }

    pub fn show_markdown(&mut self) {
        self.code_tab = "markdown".into();
    }

    pub fn show_json(&mut self) {
        self.code_tab = "json".into();
    }

    pub fn unmount_tasks(&mut self) {
        self.replace_seed(
            false,
            "Removed task nodes — their typed components unmounted",
        );
    }

    pub fn restore_demo(&mut self) {
        self.replace_seed(true, "Restored the complete typed document");
    }

    pub fn on_search_input(&mut self, event: InputEvent) {
        let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        else {
            self.search_status = "Search input target was unavailable.".into();
            return;
        };
        self.run_search(input.value());
    }

    pub fn demo_stale_search(&mut self) {
        let query = if self.search_query.trim().is_empty() {
            "table".to_string()
        } else {
            self.search_query.clone()
        };
        self.run_search(query);
    }

    pub fn run_search_result(&mut self, action: String) {
        match action.as_str() {
            "bold" => self.toggle_bold(),
            "italic" => self.toggle_italic(),
            "tag_info" => self.insert_info_tag(),
            "tag_success" => self.insert_success_tag(),
            "table" => self.insert_table(),
            "column" => self.widen_column(),
            "row" => self.heighten_row(),
            _ => self.status = format!("Unknown command result `{action}`"),
        }
    }
}

impl ExternalBlocksDemo {
    fn editor(&self) -> Option<RichTextEditor> {
        let scope = current_scope_id()?;
        let root = pocopine::refs::get_on(scope, "root")?;
        RichTextEditor::find(&root)
    }

    fn dispatch(&mut self, request: CommandRequest, success: &str) {
        let Some(editor) = self.editor() else {
            self.error = "rich-text editor is unavailable".into();
            return;
        };
        match editor.dispatch(request) {
            Ok(()) => {
                self.status = success.into();
                self.error.clear();
            }
            Err(error) => self.error = error.to_string(),
        }
    }

    fn custom(&mut self, name: &str, args: Value, success: &str) {
        self.dispatch(
            CommandRequest::Custom {
                name: name.into(),
                args,
            },
            success,
        );
    }

    fn insert_tag(&mut self, id: &str, label: &str, kind: TagKind) {
        self.custom(
            "insert_tag",
            json!({ "id": id, "label": label, "kind": kind }),
            "Inserted a typed tag at the live selection",
        );
    }

    fn run_editor(
        &mut self,
        label: &str,
        action: fn(&RichTextEditor) -> Result<(), pine_richtext::view::EditorError>,
    ) {
        let Some(editor) = self.editor() else {
            self.error = "rich-text editor is unavailable".into();
            return;
        };
        match action(&editor) {
            Ok(()) => {
                self.status = label.into();
                self.error.clear();
            }
            Err(error) => self.error = error.to_string(),
        }
    }

    fn replace_seed(&mut self, include_tasks: bool, success: &str) {
        let Some(editor) = self.editor() else {
            self.error = "rich-text editor is unavailable".into();
            return;
        };
        let runtime = runtime::registry::resolve(Some(RUNTIME_NAME));
        let doc = match seed_document(runtime.schema(), include_tasks) {
            Ok(doc) => doc,
            Err(error) => {
                self.error = error;
                return;
            }
        };
        let value = match serde_json::to_value(doc) {
            Ok(value) => value,
            Err(error) => {
                self.error = error.to_string();
                return;
            }
        };
        if let Err(error) = editor.set::<Doc>(&value) {
            self.error = error.to_string();
            return;
        }
        let (semantic_json, markdown, error) = outputs_for_value(&value, &runtime);
        self.semantic_json = semantic_json;
        self.markdown = markdown;
        self.error = error;
        self.status = success.into();
    }

    fn run_search(&mut self, query: String) {
        self.search_query = query.clone();
        self.search_results = command_options(&query);
        if query.trim().is_empty() {
            self.search_status =
                "Type a command query; each keystroke supersedes the prior token.".into();
            return;
        }

        let Some(scope) = current_scope_id() else {
            self.search_status = "Component scope is unavailable.".into();
            return;
        };
        let outcome = DEMO_RESOURCES.with(|resources| {
            let resources = resources.borrow();
            let resources = resources
                .get(&scope)
                .ok_or_else(|| "bubble-menu controller is unavailable".to_string())?;
            let old = resources
                .bubble
                .begin_search(format!("{query} (slow)"))
                .map_err(|error| error.to_string())?;
            let current = resources
                .bubble
                .begin_search(query.clone())
                .map_err(|error| error.to_string())?;
            let old_query = old.query().to_string();
            let current_query = current.query().to_string();
            let search = resources
                .bubble
                .search()
                .ok_or_else(|| "bubble-menu search is disabled".to_string())?;
            let stale = search.finish(old, "late provider result");
            let accepted = search
                .finish(current, "current provider result")
                .map_err(|error| error.to_string())?;
            if stale.is_ok() {
                return Err("the superseded search result was incorrectly accepted".into());
            }
            Ok(format!(
                "Rejected stale `{old_query}`; accepted `{current_query}` at {:?}.",
                accepted.selection
            ))
        });
        self.search_status = outcome.unwrap_or_else(|error| error);
    }

    fn apply_lifecycle(&mut self, counts: TaskLifecycle) {
        self.task_mounts = counts.mounts;
        self.task_syncs = counts.syncs;
        self.task_unmounts = counts.unmounts;
        self.task_active = counts.active;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ShowcaseTaskItem.poco",
    style = "task_item.css",
    role = "scope",
    display = "list-item"
)]
pub struct ShowcaseTaskItem {
    pub checked: bool,
    pub sync_count: u64,
    pub error: String,
}

impl RichTextNodeView<TaskItemNode> for ShowcaseTaskItem {
    fn sync_node(&mut self, update: NodeViewUpdate<TaskItemAttrs>) -> Result<(), NodeViewError> {
        self.checked = update.attrs.checked;
        self.sync_count = self.sync_count.saturating_add(1);
        self.error.clear();
        record_task_lifecycle(TaskLifecycleEvent::Sync);
        Ok(())
    }
}

#[handlers]
impl ShowcaseTaskItem {
    fn on_mount(&mut self) {
        record_task_lifecycle(TaskLifecycleEvent::Mount);
    }

    fn on_unmount(&mut self) {
        record_task_lifecycle(TaskLifecycleEvent::Unmount);
    }

    pub fn toggle(&mut self, #[context] node: NodeViewHandle<TaskItemNode>) {
        if let Err(error) = node.update_attrs(|attrs| attrs.checked = !attrs.checked) {
            self.error = error.to_string();
        }
    }

    /// Dispatch from the component that the transaction immediately removes.
    /// This is the smallest end-to-end proof that Pocopine defers editor
    /// callbacks until the component handler's mutable scope borrow is over.
    pub fn delete_self(&mut self, #[context] node: NodeViewHandle<TaskItemNode>) {
        if let Err(error) = node.delete() {
            self.error = error.to_string();
        }
    }
}

#[derive(Clone, Copy)]
enum TaskLifecycleEvent {
    Mount,
    Sync,
    Unmount,
}

fn record_task_lifecycle(event: TaskLifecycleEvent) {
    TASK_LIFECYCLE.with(|lifecycle| {
        let mut lifecycle = lifecycle.borrow_mut();
        match event {
            TaskLifecycleEvent::Mount => {
                lifecycle.mounts = lifecycle.mounts.saturating_add(1);
                lifecycle.active = lifecycle.active.saturating_add(1);
            }
            TaskLifecycleEvent::Sync => {
                lifecycle.syncs = lifecycle.syncs.saturating_add(1);
            }
            TaskLifecycleEvent::Unmount => {
                lifecycle.unmounts = lifecycle.unmounts.saturating_add(1);
                lifecycle.active = lifecycle.active.saturating_sub(1);
            }
        }
    });
    publish_task_lifecycle();
}

fn task_lifecycle() -> TaskLifecycle {
    TASK_LIFECYCLE.with(|lifecycle| *lifecycle.borrow())
}

fn publish_task_lifecycle() {
    let handle = DEMO_HANDLE.with(|slot| slot.borrow().clone());
    let Some(handle) = handle else { return };
    pocopine::tick::next(move || {
        let counts = task_lifecycle();
        handle.update(move |state| state.apply_lifecycle(counts));
    });
}

struct SeedPayload {
    state: Value,
    json: String,
    markdown: String,
}

fn seed_payload(include_tasks: bool) -> Result<SeedPayload, String> {
    let runtime = runtime::registry::resolve(Some(RUNTIME_NAME));
    let doc = seed_document(runtime.schema(), include_tasks)?;
    let doc_value = serde_json::to_value(&doc).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(&doc_value).map_err(|error| error.to_string())?;
    let markdown = runtime
        .markdown_serializer()
        .serialize(&doc)
        .map_err(|error| error.to_string())?;
    let state = EditorState::create(
        EditorStateConfig::new(runtime.schema().clone(), doc).plugins(runtime.plugins().to_vec()),
    )
    .map_err(|error| error.to_string())?
    .to_json()
    .map_err(|error| error.to_string())?;
    Ok(SeedPayload {
        state,
        json,
        markdown,
    })
}

fn seed_document(schema: &Schema, include_tasks: bool) -> Result<Node, String> {
    let mut blocks = vec![
        paragraph(
            schema,
            vec![
                text(
                    schema,
                    "Typed external blocks keep semantic data in the document and UI state in components. ",
                )?,
                tag(schema, "architecture", "Architecture", TagKind::Info)?,
                text(schema, " and ")?,
                tag(schema, "runtime-safe", "Runtime safe", TagKind::Success)?,
                text(
                    schema,
                    " are real inline atoms — use ArrowLeft/ArrowRight and Delete around them.",
                )?,
            ],
        )?,
        paragraph(
            schema,
            vec![text(
                schema,
                "Select this sentence to open the BubbleMenu, then type a command query to see stale provider results rejected.",
            )?],
        )?,
    ];

    if include_tasks {
        blocks.push(task_list(
            schema,
            [
                (true, "Task attrs update through a typed NodeViewHandle"),
                (
                    false,
                    "Unmount and restore this list to inspect lifecycle counts",
                ),
            ],
        )?);
    }

    blocks.push(paragraph(
        schema,
        vec![text(
            schema,
            "Table playground — drag a cell edge to resize; use the A/B/C and 1/2/3 selectors; modifier-drag across cells for a rectangular selection.",
        )?],
    )?);
    blocks.push(table(schema)?);
    blocks.push(paragraph(
        schema,
        vec![
            text(
                schema,
                "Every edit updates the semantic JSON and Markdown panels. ",
            )?,
            tag(
                schema,
                "no-html-strings",
                "No HTML strings",
                TagKind::Warning,
            )?,
        ],
    )?);

    schema
        .node("doc", Attrs::new(), Fragment::from(blocks))
        .map_err(|error| error.to_string())
}

fn text(schema: &Schema, value: &str) -> Result<Node, String> {
    schema
        .text(value.to_string(), Vec::new())
        .map_err(|error| error.to_string())
}

fn paragraph(schema: &Schema, content: Vec<Node>) -> Result<Node, String> {
    schema
        .node("paragraph", Attrs::new(), Fragment::from(content))
        .map_err(|error| error.to_string())
}

fn tag(schema: &Schema, id: &str, label: &str, kind: TagKind) -> Result<Node, String> {
    let attrs = TagAttrs::new(id, label)
        .map_err(|error| error.to_string())?
        .with_kind(kind)
        .to_attrs()
        .map_err(|error| error.to_string())?;
    schema
        .node(TagNode::NAME, attrs, Fragment::empty())
        .map_err(|error| error.to_string())
}

fn task_list(
    schema: &Schema,
    items: impl IntoIterator<Item = (bool, &'static str)>,
) -> Result<Node, String> {
    let items = items
        .into_iter()
        .map(|(checked, label)| {
            let attrs = encode_attrs(&TaskItemAttrs { checked })?;
            let content = paragraph(schema, vec![text(schema, label)?])?;
            schema
                .node(TaskItemNode::NAME, attrs, Fragment::from(content))
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    schema
        .node("task_list", Attrs::new(), Fragment::from(items))
        .map_err(|error| error.to_string())
}

fn table(schema: &Schema) -> Result<Node, String> {
    let rows = vec![
        table_row(
            schema,
            true,
            Some(44),
            [
                ("Semantic block", Some(TableAlignment::Left)),
                ("Typed capability", Some(TableAlignment::Center)),
                ("State", Some(TableAlignment::Center)),
            ],
        )?,
        table_row(
            schema,
            false,
            Some(48),
            [
                ("Task item", Some(TableAlignment::Left)),
                ("Live attrs + lifecycle", Some(TableAlignment::Center)),
                ("Mounted", Some(TableAlignment::Center)),
            ],
        )?,
        table_row(
            schema,
            false,
            Some(48),
            [
                ("Table", Some(TableAlignment::Left)),
                ("Resize + cell selection", Some(TableAlignment::Center)),
                ("Editable", Some(TableAlignment::Center)),
            ],
        )?,
    ];
    let attrs = encode_attrs(&TableAttrs {
        column_widths: vec![Some(190), Some(250), Some(150)],
    })?;
    schema
        .node(TableNode::NAME, attrs, Fragment::from(rows))
        .map_err(|error| error.to_string())
}

fn table_row<const N: usize>(
    schema: &Schema,
    header: bool,
    height: Option<u32>,
    cells: [(&str, Option<TableAlignment>); N],
) -> Result<Node, String> {
    let cells = cells
        .into_iter()
        .map(|(label, alignment)| table_cell(schema, header, label, alignment))
        .collect::<Result<Vec<_>, _>>()?;
    schema
        .node(
            TableRowNode::NAME,
            encode_attrs(&TableRowAttrs { height })?,
            Fragment::from(cells),
        )
        .map_err(|error| error.to_string())
}

fn table_cell(
    schema: &Schema,
    header: bool,
    label: &str,
    alignment: Option<TableAlignment>,
) -> Result<Node, String> {
    let (node_type, attrs) = if header {
        (
            TableHeaderCellNode::NAME,
            encode_attrs(&TableHeaderCellAttrs { alignment })?,
        )
    } else {
        (
            TableCellNode::NAME,
            encode_attrs(&TableCellAttrs { alignment })?,
        )
    };
    schema
        .node(node_type, attrs, Fragment::from(text(schema, label)?))
        .map_err(|error| error.to_string())
}

fn encode_attrs(value: &impl Serialize) -> Result<Attrs, String> {
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn outputs_for_value(value: &Value, runtime: &EditorRuntime) -> (String, String, String) {
    let semantic_json =
        serde_json::to_string_pretty(value).unwrap_or_else(|error| error.to_string());
    let markdown = serde_json::from_value::<Node>(value.clone())
        .map_err(|error| error.to_string())
        .and_then(|doc| {
            runtime
                .markdown_serializer()
                .serialize(&doc)
                .map_err(|error| error.to_string())
        });
    match markdown {
        Ok(markdown) => (semantic_json, markdown, String::new()),
        Err(error) => (semantic_json, String::new(), error),
    }
}

fn summarize_selection(snapshot: &SelectionSnapshot) -> String {
    let kind = match &snapshot.selection {
        Selection::Text { anchor, head } if anchor == head => format!("caret at {head}"),
        Selection::Text { anchor, head } => format!("text {anchor} → {head}"),
        Selection::Node { anchor } => format!("node at {anchor}"),
        Selection::Cells {
            anchor_cell,
            head_cell,
        } => format!("cells {anchor_cell} → {head_cell}"),
        Selection::All => "whole document".into(),
    };
    let marks = if snapshot.active_mark_names.is_empty() {
        "no active marks".into()
    } else {
        snapshot.active_mark_names.join(", ")
    };
    let blocks = if snapshot.enclosing_block_types.is_empty() {
        "document".into()
    } else {
        snapshot.enclosing_block_types.join(" › ")
    };
    format!("{kind} · {marks} · {blocks}")
}

fn command_options(query: &str) -> Vec<SearchOption> {
    let query = query.trim().to_ascii_lowercase();
    [
        ("bold", "Bold selection", "Mark", "bold"),
        ("italic", "Italic selection", "Mark", "italic"),
        ("tag-info", "Insert info tag", "Typed atom", "tag_info"),
        (
            "tag-success",
            "Insert verified tag",
            "Typed atom",
            "tag_success",
        ),
        ("table", "Insert 3 × 3 table", "Typed block", "table"),
        ("column", "Set column width", "Table attrs", "column"),
        ("row", "Set row height", "Table attrs", "row"),
    ]
    .into_iter()
    .filter(|(_, label, hint, _)| {
        query.is_empty()
            || label.to_ascii_lowercase().contains(&query)
            || hint.to_ascii_lowercase().contains(&query)
    })
    .map(|(key, label, hint, action)| SearchOption {
        key: key.into(),
        label: label.into(),
        hint: hint.into(),
        action: action.into(),
    })
    .collect()
}

fn build_runtime() -> Result<Arc<EditorRuntime>, String> {
    RuntimeBuilder::new()
        .name(RUNTIME_NAME)
        .with_view(TaskListExtension::new().with_typed_node_view::<ShowcaseTaskItem>())
        .with_view(TablesExtension)
        .with_view(TagsExtension)
        .try_build()
        .map_err(|error| error.to_string())
}

#[wasm_bindgen(start)]
pub fn main() {
    let runtime = build_runtime().expect("external-blocks runtime is valid");
    runtime::registry::register(RUNTIME_NAME, runtime);

    App::new()
        .register::<ExternalBlocksDemo>()
        .register::<PineRichTextRoot>()
        .run();
}
