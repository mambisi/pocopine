//! Minimal pine-richtext demo.
//!
//! Wires a `<pine-rich-text-root>` surface to a parent `<Editor>` that
//! holds the doc JSON. Toolbar buttons read the live DOM selection
//! out of the editor surface (so Bold/Italic format the selected text,
//! not the whole document), build an `EditorState` with that selection
//! injected, dispatch a `pine_richtext::commands::*` call, and write
//! the new state JSON back into the signal — which the
//! `<pine-rich-text-root>` re-renders.
//!
//! The toolbar buttons use `@mousedown.prevent` (set on the template)
//! so clicking them doesn't move keyboard focus off the surface and
//! collapse the user's selection.

use std::sync::Arc;

use pine_richtext::commands::BoxedCommand;
use pine_richtext::extension;
use pine_richtext::extension::{NamedCommand, RichTextExtension};
use pine_richtext::extensions::{
    CoreMarksExtension, MarkdownShortcutsExtension, SmartTypographyExtension, TaskListExtension,
};
use pine_richtext::history::history_plugin;
use pine_richtext::model::{Attrs, MarkPolicy, NodeSpec};
use pine_richtext::runtime::{self, RuntimeBuilder};
use pine_richtext::schema_basic;
use pine_richtext::state::{EditorState, EditorStateConfig, Plugin, Selection, Transaction};
use pine_richtext::view::root::{
    CommandRequest, COMMAND_EVENT, EXPORT_MARKDOWN_REQUEST_EVENT, EXPORT_MARKDOWN_RESULT_EVENT,
};
use pine_richtext::view::PineRichTextRoot;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsCast;
use web_sys::{CustomEvent, CustomEventInit, Element, Event};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Editor {
    /// One-way seed document passed to the surface via
    /// `pp-bind:initial-doc`. The surface copies it into its own
    /// authoritative `doc` field on first mount and ignores further
    /// writes — so the parent never becomes a source of truth that
    /// could race the surface's reactive state.
    pub initial_doc: Value,
    /// Latest markdown export. Rendered into the demo's "Exported
    /// markdown" `<pre>` block whenever the user clicks Export MD.
    /// Updated by the `pine:richtext:export-markdown-result`
    /// listener installed in `on_ready` — the surface owns the
    /// serialization (it has the live state) so the Editor never
    /// has to mirror the doc.
    pub exported_markdown: String,
}

#[handlers]
impl Editor {
    fn on_mount(&mut self) {
        if self.initial_doc.is_null() {
            self.initial_doc = initial_doc_json();
        }
    }

    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = refs.get("root") else {
            return;
        };
        let handle = this::<Editor>();
        install_task_toggle_listener(root.clone(), handle.clone());
        install_export_markdown_listener(root, handle);
    }

    /// Toggle strong (Bold) on the currently selected text.
    pub fn toggle_bold(&mut self) {
        Self::dispatch_command(CommandRequest::ToggleMark {
            mark: "strong".into(),
        });
    }

    /// Toggle em (Italic) on the currently selected text.
    pub fn toggle_em(&mut self) {
        Self::dispatch_command(CommandRequest::ToggleMark { mark: "em".into() });
    }

    /// Toggle code on the currently selected text.
    pub fn toggle_code(&mut self) {
        Self::dispatch_command(CommandRequest::ToggleMark {
            mark: "code".into(),
        });
    }

    /// Convert the block containing the cursor (or every block in the
    /// selection) to a level-1 heading.
    pub fn make_h1(&mut self) {
        let mut attrs = Attrs::new();
        attrs.insert("level".to_string(), serde_json::json!(1));
        Self::dispatch_command(CommandRequest::SetBlockType {
            node_type: "heading".into(),
            attrs,
        });
    }

    /// Convert the affected blocks to level-2 headings.
    pub fn make_h2(&mut self) {
        let mut attrs = Attrs::new();
        attrs.insert("level".to_string(), serde_json::json!(2));
        Self::dispatch_command(CommandRequest::SetBlockType {
            node_type: "heading".into(),
            attrs,
        });
    }

    /// Convert the affected blocks back to plain paragraphs.
    pub fn make_paragraph(&mut self) {
        Self::dispatch_command(CommandRequest::SetBlockType {
            node_type: "paragraph".into(),
            attrs: Attrs::new(),
        });
    }

    /// Wrap the affected blocks in a blockquote.
    pub fn wrap_in_blockquote(&mut self) {
        Self::dispatch_command(CommandRequest::WrapIn {
            node_type: "blockquote".into(),
            attrs: Attrs::new(),
        });
    }

    /// Wrap the affected blocks in a bullet list.
    pub fn wrap_in_bullet_list(&mut self) {
        Self::dispatch_command(CommandRequest::WrapInList {
            list_type: "bullet_list".into(),
            item_type: "list_item".into(),
            attrs: Attrs::new(),
        });
    }

    /// Wrap the affected blocks in an ordered list.
    pub fn wrap_in_ordered_list(&mut self) {
        Self::dispatch_command(CommandRequest::WrapInList {
            list_type: "ordered_list".into(),
            item_type: "list_item".into(),
            attrs: Attrs::new(),
        });
    }

    /// Wrap the affected blocks in a task (checklist) list.
    pub fn wrap_in_task_list(&mut self) {
        Self::dispatch_command(CommandRequest::WrapInList {
            list_type: "task_list".into(),
            item_type: "task_item".into(),
            attrs: Attrs::new(),
        });
    }

    /// Lift the affected blocks out of their wrapper.
    pub fn lift_block(&mut self) {
        Self::dispatch_command(CommandRequest::Lift);
    }

    /// Undo the last edit.
    pub fn undo(&mut self) {
        Self::dispatch_command(CommandRequest::Undo);
    }

    /// Redo the most recently undone edit.
    pub fn redo(&mut self) {
        Self::dispatch_command(CommandRequest::Redo);
    }

    /// Reset the doc to the demo's starting content.
    pub fn reset(&mut self) {
        Self::dispatch_command(CommandRequest::ReplaceState {
            doc: initial_doc_json(),
        });
    }

    /// Parse the import-markdown textarea's contents via the
    /// default runtime's [`pine_richtext::markdown::MarkdownParser`]
    /// and replace the surface's doc with the result. Routes
    /// through `CommandRequest::ReplaceState` so the swap lands in
    /// the same event pipeline as Reset and other state-
    /// replacement operations.
    ///
    /// Reads the textarea straight from the DOM via the
    /// `import_textarea` ref instead of binding `pp-model:value`
    /// — `<textarea>` doesn't emit the `pp:update:value` channel
    /// that `pp-model:value` listens for, so the binding wouldn't
    /// propagate keystrokes back to `self.import_input`.
    pub fn import_markdown(&mut self) {
        let Some(scope) = pocopine::current_scope_id() else {
            return;
        };
        let Some(el) = pocopine::refs::get_on(scope, "import_textarea") else {
            return;
        };
        let Ok(textarea) = el.dyn_into::<web_sys::HtmlTextAreaElement>() else {
            return;
        };
        let md = textarea.value();
        if md.is_empty() {
            return;
        }
        let runtime = runtime::registry::default();
        let parser = runtime.markdown_parser();
        let doc_node = match parser.parse(&md, runtime.schema()) {
            Ok(node) => node,
            Err(err) => {
                self.exported_markdown = format!("(import error: {err})");
                return;
            }
        };
        let state = match EditorState::create(
            EditorStateConfig::new(runtime.schema().clone(), doc_node).plugins(demo_plugins()),
        ) {
            Ok(state) => state,
            Err(err) => {
                self.exported_markdown = format!("(import error: {err})");
                return;
            }
        };
        let Ok(state_json) = state.to_json() else {
            return;
        };
        Self::dispatch_command(CommandRequest::ReplaceState { doc: state_json });
    }

    /// Ask the surface to serialize its current doc to markdown.
    /// Fires `pine:richtext:export-markdown` at the surface — the
    /// surface runs `EditorRuntime::markdown_serializer()` against
    /// its live state and responds with a
    /// `pine:richtext:export-markdown-result` CustomEvent whose
    /// `detail.markdown` is captured into `self.exported_markdown`
    /// by the listener installed in `on_ready`.
    ///
    /// Going through the surface (instead of mirroring its doc
    /// here) means typing-then-clicking exports the latest typed
    /// content with no `tick::next` mirror lag.
    pub fn export_markdown(&mut self) {
        let Some(surface) = find_surface() else {
            return;
        };
        let init = CustomEventInit::new();
        init.set_bubbles(true);
        let Ok(event) = CustomEvent::new_with_event_init_dict(EXPORT_MARKDOWN_REQUEST_EVENT, &init)
        else {
            return;
        };
        let _ = surface.dispatch_event(&event);
    }
}

impl Editor {
    /// Dispatch a [`CommandRequest`] to the editor surface as a
    /// CustomEvent. The surface runs the command through its own
    /// `state_provider`, which reads the live (child-owned) doc and
    /// the live DOM selection — sidestepping the `pp-model` round-trip
    /// that would otherwise have the parent's mirrored `doc` lag a
    /// `tick::next` behind any typing the user just did.
    fn dispatch_command(request: CommandRequest) {
        let Some(surface) = find_surface() else {
            return;
        };
        let Ok(detail) = serde_wasm_bindgen::to_value(&request) else {
            return;
        };
        let init = CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail);
        let Ok(event) = CustomEvent::new_with_event_init_dict(COMMAND_EVENT, &init) else {
            return;
        };
        let _ = surface.dispatch_event(&event);
    }

    /// Flip the `checked` attribute on the `task_item` at `pos`. Called
    /// from the `pine:task-toggle` custom event dispatched by the
    /// `<pine-task-item>` node-view component. Reuses the surface's
    /// command event so the toggle lands in the same transaction
    /// pipeline as the toolbar — no stale-doc race possible.
    fn toggle_task_checked(&mut self, pos: usize, checked: bool) {
        Self::dispatch_command(CommandRequest::SetNodeAttr {
            pos,
            attr: "checked".into(),
            value: json!(checked),
        });
    }
}

/// Listen for the `pine:richtext:export-markdown-result` custom
/// event the surface emits after processing an export request.
/// Captures the markdown string into the editor's
/// `exported_markdown` field so the template can render it into
/// `<pre data-test="exported-markdown">`.
fn install_export_markdown_listener(event_target: Element, handle: pocopine::Handle<Editor>) {
    let cb = Closure::wrap(Box::new(move |event: Event| {
        let Ok(custom) = event.dyn_into::<CustomEvent>() else {
            return;
        };
        let text = custom
            .detail()
            .as_string()
            .unwrap_or_else(|| "(export returned non-string detail)".to_string());
        let handle = handle.clone();
        pocopine::tick::next(move || {
            handle.update(move |editor: &mut Editor| {
                editor.exported_markdown = text;
            });
        });
    }) as Box<dyn FnMut(Event)>);
    let _ = event_target.add_event_listener_with_callback(
        EXPORT_MARKDOWN_RESULT_EVENT,
        cb.as_ref().unchecked_ref(),
    );
    cb.forget();
}

/// Listen for the `pine:task-toggle` custom event bubbled up from
/// `<pine-task-item>` node-view components and dispatch the
/// corresponding model update.
fn install_task_toggle_listener(event_target: Element, handle: pocopine::Handle<Editor>) {
    let cb = Closure::wrap(Box::new(move |event: Event| {
        let Ok(custom) = event.dyn_into::<CustomEvent>() else {
            return;
        };
        let detail = custom.detail();
        let Ok(payload) = serde_wasm_bindgen::from_value::<TaskTogglePayload>(detail) else {
            return;
        };
        let handle = handle.clone();
        pocopine::tick::next(move || {
            handle.update(move |editor: &mut Editor| {
                editor.toggle_task_checked(payload.pos, payload.checked);
            });
        });
    }) as Box<dyn FnMut(Event)>);
    let _ = event_target
        .add_event_listener_with_callback("pine:task-toggle", cb.as_ref().unchecked_ref());
    cb.forget();
}

#[derive(Deserialize, Serialize)]
struct TaskTogglePayload {
    pos: usize,
    checked: bool,
}

/// Find the doc editor surface element in the DOM. The demo now hosts
/// two `<pine-rich-text-root>` mounts — the kitchen-sink doc editor
/// (no `runtime` attribute) and the minimal comment editor
/// (`runtime="comment"`). The toolbar targets the doc editor by
/// selecting the FIRST surface that has no `runtime` attribute set.
fn find_surface() -> Option<Element> {
    let window = web_sys::window()?;
    let document = window.document()?;
    document
        .query_selector("pine-rich-text-root:not([runtime]) .pine-rich-text")
        .ok()
        .flatten()
        .or_else(|| {
            document
                .query_selector("pine-rich-text-root:not([runtime])")
                .ok()
                .flatten()
                .and_then(|el| el.dyn_into::<Element>().ok())
        })
}

fn initial_doc_json() -> Value {
    let schema = schema_basic::schema();
    let p1 = schema_basic::paragraph(vec![schema_basic::text(
        "Hello, pine-richtext.",
        Vec::new(),
    )
    .unwrap()])
    .unwrap();
    let p2 = schema_basic::paragraph(vec![
        schema_basic::text("Select some text and use the toolbar: ", Vec::new()).unwrap(),
        schema_basic::text("Bold", vec![schema_basic::strong().unwrap()]).unwrap(),
        schema_basic::text(", ", Vec::new()).unwrap(),
        schema_basic::text("italic", vec![schema_basic::em().unwrap()]).unwrap(),
        schema_basic::text(
            ", code, headings, blockquote, lift — all model commands.",
            Vec::new(),
        )
        .unwrap(),
    ])
    .unwrap();
    let checklist = schema_basic::task_list(vec![
        schema_basic::task_item(
            true,
            vec![schema_basic::paragraph(vec![schema_basic::text(
                "Schema with task_list / task_item",
                Vec::new(),
            )
            .unwrap()])
            .unwrap()],
        )
        .unwrap(),
        schema_basic::task_item(
            false,
            vec![schema_basic::paragraph(vec![schema_basic::text(
                "Click the box to toggle this item",
                Vec::new(),
            )
            .unwrap()])
            .unwrap()],
        )
        .unwrap(),
    ])
    .unwrap();
    let document = schema_basic::doc(vec![p1, p2, checklist]).unwrap();
    let state =
        EditorState::create(EditorStateConfig::new(schema, document).plugins(demo_plugins()))
            .unwrap();
    state.to_json().unwrap()
}

/// Plugin set the demo materializes states with. Matches what
/// `PineRichTextRoot` uses internally so the history JSON written by
/// the surface's typing path round-trips here.
fn demo_plugins() -> Vec<Plugin> {
    vec![history_plugin()]
}

// Silence the unused-import warning when read_dom_selection isn't
// referenced inside the `commit` path (it is, via state_with_live_selection,
// but the borrow-checker is conservative).
#[allow(unused_imports)]
use pine_richtext::commands::Command as _;

/// Node-view component for `task_item`. The renderer emits
/// `<pine-task-item data-pos="N" data-checked="true|false">…inline
/// content…</pine-task-item>`; the component layers a checkbox button
/// over the inline content via a slot, and on click dispatches a
/// bubbling `pine:task-toggle` CustomEvent carrying the position and
/// the new checked value. The parent `<Editor>` listens at the surface
/// for that event and updates the model.
///
/// This is pocopine's analogue of Tiptap's React NodeView: the
/// component owns its wrapper chrome (checkbox, future drag handle,
/// future delete button) while pine-richtext keeps owning the inline
/// content under `[data-pine-richtext-content]`. Keep the component
/// template free of literal whitespace between chrome children and at
/// EOF; those text nodes become cursor targets and line boxes in the
/// contenteditable surface.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTaskItem.poco", role = "scope", display = "list-item")]
pub struct PineTaskItem {
    /// Mirrors the model `task_item`'s `checked` attribute via the
    /// `data-checked` HTML attribute emitted by the renderer.
    #[prop]
    pub checked: bool,
}

#[handlers]
impl PineTaskItem {
    pub fn toggle(&mut self) {
        let Some(scope) = pocopine::current_scope_id() else {
            return;
        };
        let Some(host) = pocopine::refs::get_on(scope, "root") else {
            return;
        };
        let node_view_host = host.parent_element().unwrap_or_else(|| host.clone());
        let current = node_view_host.get_attribute("data-checked").as_deref() == Some("true");
        let next = !current;
        let Some(pos) = node_view_host
            .get_attribute("data-pos")
            .and_then(|s: String| s.parse::<usize>().ok())
        else {
            return;
        };
        let Ok(detail) = serde_wasm_bindgen::to_value(&TaskTogglePayload { pos, checked: next })
        else {
            return;
        };
        let init = CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail);
        let Ok(event) = CustomEvent::new_with_event_init_dict("pine:task-toggle", &init) else {
            return;
        };
        let _ = host.dispatch_event(&event);
    }
}

/// Demo extension contributing the **minimal** schema for the
/// comment runtime: only `doc`, `paragraph`, and `text`. No headings,
/// blockquotes, code blocks, lists, task items, horizontal rules,
/// images, or hard breaks. A caller dispatching `set_block_type`
/// against the comment editor with `node_type: "heading"` (or any
/// other unsupported type) fails at schema lookup — the runtime
/// genuinely cannot represent non-paragraph blocks.
struct CommentSchemaExtension;

impl RichTextExtension for CommentSchemaExtension {
    fn name(&self) -> &str {
        "comment-schema"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            NodeSpec::new("doc").content("paragraph+"),
            NodeSpec::new("paragraph")
                .group("block")
                .content("inline*")
                .marks(MarkPolicy::All),
            NodeSpec::new("text").group("inline").inline(),
        ]
    }
}

/// Demo extension that contributes a single named command,
/// `comment_submit`, which inserts a sentinel string into the doc.
/// Used by the comment runtime to prove per-instance commands: the
/// same `{ kind: "custom", name: "comment_submit" }` event fires
/// against the comment editor (inserts text) but is a silent no-op
/// against the doc editor (no such command in its runtime).
struct CommentRuntimeExtension;

impl RichTextExtension for CommentRuntimeExtension {
    fn name(&self) -> &str {
        "comment-runtime"
    }

    fn commands(&self) -> Vec<(String, NamedCommand)> {
        let factory: NamedCommand = Arc::new(|_args| {
            Some(Box::new(|state: &EditorState| -> Option<Transaction> {
                let mut tr = state.tr();
                tr.insert_text("✓submitted").ok()?;
                Some(tr)
            }) as BoxedCommand)
        });
        vec![("comment_submit".into(), factory)]
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    // The "default" runtime — the full kitchen-sink doc editor. The
    // `extension::register(...)` path is the only way to customize the
    // *default* runtime's extension list (named runtimes use
    // `runtime::registry::register`). Soft-deprecated since Phase 4b
    // C5; the deprecation warning is suppressed at this single
    // call-site because no replacement API exists yet for
    // "customize the default runtime."
    #[allow(deprecated)]
    extension::register(Box::new(
        TaskListExtension::new().with_node_view::<PineTaskItem>(),
    ));

    // Phase 5 C3 — register the predefined input-rule extensions
    // for the doc editor. `SmartTypographyExtension` adds em-dash,
    // ellipsis, and the four smart-quote variants;
    // `MarkdownShortcutsExtension` adds `# ` → heading, `* ` →
    // bullet list, `> ` → blockquote, ``` ``` ``` → code block, and
    // `1. ` → ordered list. The comment runtime intentionally does
    // NOT register markdown shortcuts (its minimal schema rejects
    // headings/lists anyway) but DOES get smart typography below.
    #[allow(deprecated)]
    extension::register(Box::new(SmartTypographyExtension));
    #[allow(deprecated)]
    extension::register(Box::new(MarkdownShortcutsExtension));

    // The "comment" runtime — TRULY minimal: only `doc`, `paragraph`,
    // and `text` plus the standard marks. No headings, blockquotes,
    // code blocks, lists, task items, horizontal rules, images, or
    // hard breaks. No history plugin. Schema-level enforcement: any
    // `set_block_type` / `wrap_in` command targeting an unsupported
    // node type fails at schema lookup.
    let comment = RuntimeBuilder::new()
        .name("comment")
        .without_defaults()
        .with(CommentSchemaExtension)
        .with(CoreMarksExtension)
        .with(CommentRuntimeExtension)
        // Smart typography is a universal nicety — comment box users
        // get em-dash + smart quotes too. No markdown shortcuts
        // (the comment schema rejects headings/lists/blockquotes
        // anyway).
        .with(SmartTypographyExtension)
        .build();
    runtime::registry::register("comment", comment);

    App::new()
        .register::<Editor>()
        .register::<PineRichTextRoot>()
        .register::<PineTaskItem>()
        .run();
}

#[allow(dead_code)]
fn _force_unused_selection_type(_: Selection) {}
