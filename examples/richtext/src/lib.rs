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
use pine_richtext::extension::{NamedCommand, RichTextExtension};
use pine_richtext::extensions::{
    CoreMarksExtension, MarkdownShortcutsExtension, SmartTypographyExtension, TaskItemAttrs,
    TaskItemNode, TaskListExtension,
};
use pine_richtext::history::history_plugin;
use pine_richtext::model::{Attrs, MarkPolicy, NodeSpec};
use pine_richtext::runtime::{self, RuntimeBuilder};
use pine_richtext::schema_basic;
use pine_richtext::state::{EditorState, EditorStateConfig, Plugin, Selection, Transaction};
use pine_richtext::view::root::CommandRequest;
use pine_richtext::view::{
    Editor as RichTextHandle, Markdown, NodeViewError, NodeViewHandle, NodeViewUpdate,
    PineRichTextRoot, RichTextNodeView,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;

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
    /// Read synchronously through the surface's typed editor handle, so the
    /// parent never has to mirror the live document.
    pub exported_markdown: String,
    /// Whether to forward debug-json to the surface. Always on for
    /// the regular demo (the smoke tests subscribe to
    /// `pine-richtext:json` events). Disabled when the URL carries
    /// `?bench=...` so perf measurements aren't dominated by the
    /// three full-state serializations per keystroke that debug-json
    /// performs (before / transaction / after).
    pub emit_debug_json: bool,
}

#[handlers]
impl Editor {
    /// Run in `on_setup` (not `on_mount`) so the values land before the
    /// `<pine-rich-text-root>` child mounts and captures its props. The
    /// surface reads `debug_json` once in its own setup; setting it
    /// after the child mount races the child's lifecycle and leaves the
    /// surface configured for the wrong mode.
    fn on_setup(&mut self) {
        let bench_spec = bench_spec_from_url();
        if self.initial_doc.is_null() {
            self.initial_doc = match bench_spec {
                Some(spec) => bench_doc_json(&spec),
                None => initial_doc_json(),
            };
        }
        // Smoke tests need debug-json; perf tests don't. Default on
        // for the kitchen-sink demo, off when bench mode is active.
        self.emit_debug_json = bench_spec.is_none();
    }

    /// Toggle strong (Bold) on the currently selected text.
    pub fn toggle_bold(&mut self) {
        self.with_editor(|e| e.toggle_mark("strong"));
    }

    /// Toggle em (Italic) on the currently selected text.
    pub fn toggle_em(&mut self) {
        self.with_editor(|e| e.toggle_mark("em"));
    }

    /// Toggle code on the currently selected text.
    pub fn toggle_code(&mut self) {
        self.with_editor(|e| e.toggle_mark("code"));
    }

    /// Convert the block containing the cursor (or every block in the
    /// selection) to a level-1 heading.
    pub fn make_h1(&mut self) {
        self.set_heading_level(1);
    }

    /// Convert the affected blocks to level-2 headings.
    pub fn make_h2(&mut self) {
        self.set_heading_level(2);
    }

    /// Convert the affected blocks back to plain paragraphs.
    pub fn make_paragraph(&mut self) {
        self.with_editor(|e| {
            e.dispatch(CommandRequest::SetBlockType {
                node_type: "paragraph".into(),
                attrs: Attrs::new(),
            })
        });
    }

    /// Wrap the affected blocks in a blockquote.
    pub fn wrap_in_blockquote(&mut self) {
        self.with_editor(|e| {
            e.dispatch(CommandRequest::WrapIn {
                node_type: "blockquote".into(),
                attrs: Attrs::new(),
            })
        });
    }

    /// Wrap the affected blocks in a bullet list.
    pub fn wrap_in_bullet_list(&mut self) {
        self.wrap_in_list("bullet_list", "list_item");
    }

    /// Wrap the affected blocks in an ordered list.
    pub fn wrap_in_ordered_list(&mut self) {
        self.wrap_in_list("ordered_list", "list_item");
    }

    /// Wrap the affected blocks in a task (checklist) list.
    pub fn wrap_in_task_list(&mut self) {
        self.wrap_in_list("task_list", "task_item");
    }

    /// Lift the affected blocks out of their wrapper.
    pub fn lift_block(&mut self) {
        self.with_editor(|e| e.dispatch(CommandRequest::Lift));
    }

    /// Undo the last edit.
    pub fn undo(&mut self) {
        self.with_editor(|e| e.undo());
    }

    /// Redo the most recently undone edit.
    pub fn redo(&mut self) {
        self.with_editor(|e| e.redo());
    }

    /// Reset the doc to the demo's starting content.
    pub fn reset(&mut self) {
        self.with_editor(|e| {
            e.dispatch(CommandRequest::ReplaceState {
                doc: initial_doc_json(),
            })
        });
    }

    /// Parse the import-markdown textarea's contents and replace
    /// the surface's doc with the result. Goes through the typed
    /// [`RichTextHandle::set`] entry point so the swap lands in the
    /// same event pipeline as Reset and other state-replacement
    /// operations.
    ///
    /// Reads the textarea straight from the DOM via the
    /// `import_textarea` ref instead of binding `pp-model:value`
    /// — `<textarea>` doesn't emit the `pp:update:value` channel
    /// that `pp-model:value` listens for.
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
        if let Some(editor) = self.editor_handle()
            && let Err(err) = editor.set::<Markdown>(&md)
        {
            self.exported_markdown = format!("(import error: {err})");
        }
    }

    /// Snapshot the surface's current doc to markdown and render
    /// it into the demo's `<pre data-test="exported-markdown">`.
    /// Synchronous round-trip through the typed
    /// [`RichTextHandle::get`] helper — no listener installation,
    /// no `tick::next` lag.
    pub fn export_markdown(&mut self) {
        let Some(editor) = self.editor_handle() else {
            return;
        };
        self.exported_markdown = match editor.get::<Markdown>() {
            Ok(md) => md,
            Err(err) => format!("(export error: {err})"),
        };
    }
}

impl Editor {
    /// Resolve a typed handle for the demo's `<pine-rich-text-root>`
    /// surface, scoped to this component's `root` ref so we never
    /// accidentally pick up the comment-editor surface hosted
    /// elsewhere on the page.
    fn editor_handle(&self) -> Option<RichTextHandle> {
        let scope = pocopine::current_scope_id()?;
        let root = pocopine::refs::get_on(scope, "root")?;
        RichTextHandle::find(&root)
    }

    fn with_editor<F>(&self, action: F)
    where
        F: FnOnce(&RichTextHandle) -> Result<(), pine_richtext::view::EditorError>,
    {
        if let Some(editor) = self.editor_handle() {
            let _ = action(&editor);
        }
    }

    fn set_heading_level(&self, level: u32) {
        let mut attrs = Attrs::new();
        attrs.insert("level".to_string(), serde_json::json!(level));
        self.with_editor(move |e| {
            e.dispatch(CommandRequest::SetBlockType {
                node_type: "heading".into(),
                attrs,
            })
        });
    }

    fn wrap_in_list(&self, list_type: &str, item_type: &str) {
        self.with_editor(|e| {
            e.dispatch(CommandRequest::WrapInList {
                list_type: list_type.into(),
                item_type: item_type.into(),
                attrs: Attrs::new(),
            })
        });
    }
}

/// Bench config parsed out of `window.location.search`. The harness
/// drives the demo via `?bench=large&paragraphs=500&words=80` so a
/// single demo binary supports many doc sizes without recompiling.
///
/// The `shape` axis selects the block layout the seed doc generates
/// — plain paragraphs by default, or typed task-item node views for
/// the `tasks` preset. Different shapes exercise
/// different reconciler paths and let us isolate custom-component
/// cost from plain-paragraph cost.
#[derive(Clone, Copy)]
struct BenchSpec {
    shape: BenchShape,
    blocks: usize,
    words_per_block: usize,
}

// `BenchShape` is only constructed inside `bench_spec_from_url`, which
// is `cfg(target_arch = "wasm32")`. Host builds see the variants as
// unconstructed dead code; allow that since the enum still needs to
// exist on host so the matching codepaths typecheck.
#[allow(dead_code)]
#[derive(Clone, Copy)]
enum BenchShape {
    Paragraphs,
    TaskItems,
}

#[cfg(target_arch = "wasm32")]
fn bench_spec_from_url() -> Option<BenchSpec> {
    let search = web_sys::window()?.location().search().ok()?;
    if search.is_empty() {
        return None;
    }
    // Strip the leading `?` so we can split on `&` cleanly.
    let trimmed = search.trim_start_matches('?');
    let mut bench: Option<&str> = None;
    let mut paragraphs: Option<usize> = None;
    let mut words: Option<usize> = None;
    for pair in trimmed.split('&') {
        let mut it = pair.splitn(2, '=');
        let key = it.next().unwrap_or("");
        let value = it.next().unwrap_or("");
        match key {
            "bench" => bench = Some(value),
            "paragraphs" => paragraphs = value.parse().ok(),
            "words" => words = value.parse().ok(),
            _ => {}
        }
    }
    let preset = bench?;
    let (default_p, default_w, shape) = match preset {
        "small" => (20, 12, BenchShape::Paragraphs),
        "medium" => (100, 40, BenchShape::Paragraphs),
        "large" => (500, 80, BenchShape::Paragraphs),
        "xl" => (2000, 80, BenchShape::Paragraphs),
        // `tasks` seeds many typed task-item component views, which
        // exercise the retained component path the plain-paragraph
        // presets skip. Default count matches `large` so cross-shape
        // comparisons are like-for-like.
        "tasks" => (500, 8, BenchShape::TaskItems),
        _ => return None,
    };
    Some(BenchSpec {
        shape,
        blocks: paragraphs.unwrap_or(default_p),
        words_per_block: words.unwrap_or(default_w),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_spec_from_url() -> Option<BenchSpec> {
    None
}

fn bench_doc_json(spec: &BenchSpec) -> Value {
    let schema = schema_basic::schema();
    let lorem = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua";
    let words: Vec<&str> = lorem.split_whitespace().collect();
    let block_text = |block_idx: usize| -> String {
        let mut s = String::with_capacity(spec.words_per_block * 8);
        for word_idx in 0..spec.words_per_block {
            if word_idx > 0 {
                s.push(' ');
            }
            s.push_str(words[(block_idx + word_idx) % words.len()]);
        }
        s.push('.');
        s
    };

    let document = match spec.shape {
        BenchShape::Paragraphs => {
            let mut blocks: Vec<pine_richtext::model::Node> = Vec::with_capacity(spec.blocks + 1);
            blocks.push(
                schema_basic::paragraph(vec![schema_basic::text(
                    format!(
                        "Bench doc ({} paragraphs × {} words). Type into me to measure keystroke latency.",
                        spec.blocks, spec.words_per_block
                    ),
                    Vec::new(),
                )
                .unwrap()])
                .unwrap(),
            );
            for i in 0..spec.blocks {
                blocks.push(
                    schema_basic::paragraph(vec![
                        schema_basic::text(block_text(i), Vec::new()).unwrap(),
                    ])
                    .unwrap(),
                );
            }
            schema_basic::doc(blocks).unwrap()
        }
        BenchShape::TaskItems => {
            // One leading paragraph for caret placement (the harness
            // looks up `p[data-pos="0"]`), then a single `task_list`
            // wrapping `spec.blocks` task items. Half checked / half
            // unchecked so reconciler attr-only patches and content
            // patches both get exercised across the run.
            let leading = schema_basic::paragraph(vec![schema_basic::text(
                format!(
                    "Bench doc ({} task items × {} words). Type into me to measure typed-view keystroke latency.",
                    spec.blocks, spec.words_per_block
                ),
                Vec::new(),
            )
            .unwrap()])
            .unwrap();
            let mut items: Vec<pine_richtext::model::Node> = Vec::with_capacity(spec.blocks);
            for i in 0..spec.blocks {
                items.push(
                    schema_basic::task_item(
                        i % 2 == 0,
                        vec![
                            schema_basic::paragraph(vec![
                                schema_basic::text(block_text(i), Vec::new()).unwrap(),
                            ])
                            .unwrap(),
                        ],
                    )
                    .unwrap(),
                );
            }
            let task_list = schema_basic::task_list(items).unwrap();
            schema_basic::doc(vec![leading, task_list]).unwrap()
        }
    };
    let state =
        EditorState::create(EditorStateConfig::new(schema, document).plugins(demo_plugins()))
            .unwrap();
    state.to_json().unwrap()
}

fn initial_doc_json() -> Value {
    let schema = schema_basic::schema();
    let p1 = schema_basic::paragraph(vec![
        schema_basic::text("Hello, pine-richtext.", Vec::new()).unwrap(),
    ])
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
            vec![
                schema_basic::paragraph(vec![
                    schema_basic::text("Schema with task_list / task_item", Vec::new()).unwrap(),
                ])
                .unwrap(),
            ],
        )
        .unwrap(),
        schema_basic::task_item(
            false,
            vec![
                schema_basic::paragraph(vec![
                    schema_basic::text("Click the box to toggle this item", Vec::new()).unwrap(),
                ])
                .unwrap(),
            ],
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

/// Typed editable view for [`TaskItemNode`]. Pine retains the native `<li>`
/// host and the semantic descendants. This component owns only the checkbox
/// shell around its compile-time-proven `pp-owned-content` outlet.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTaskItem.poco", role = "scope", display = "list-item")]
pub struct PineTaskItem {
    pub checked: bool,
    pub error: String,
}

impl RichTextNodeView<TaskItemNode> for PineTaskItem {
    fn sync_node(&mut self, update: NodeViewUpdate<TaskItemAttrs>) -> Result<(), NodeViewError> {
        self.checked = update.attrs.checked;
        self.error.clear();
        Ok(())
    }
}

#[handlers]
impl PineTaskItem {
    pub fn toggle(&mut self, #[context] node: NodeViewHandle<TaskItemNode>) {
        if let Err(error) = node.update_attrs(|attrs| attrs.checked = !attrs.checked) {
            self.error = error.to_string();
        }
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
    // Full document runtime. The typed builder pairs the exact semantic task
    // node with its component and validates the owned-content outlet before
    // any editor mounts. It also registers the component mount ABI, so the
    // app does not separately register `PineTaskItem`.
    let document = RuntimeBuilder::new()
        .name("document")
        .with_view(TaskListExtension::new().with_typed_node_view::<PineTaskItem>())
        .with(SmartTypographyExtension)
        .with(MarkdownShortcutsExtension)
        .build();
    runtime::registry::register("document", document);

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
        .run();
}

#[allow(dead_code)]
fn _force_unused_selection_type(_: Selection) {}
