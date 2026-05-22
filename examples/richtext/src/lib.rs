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
use pine_richtext::view::root::CommandRequest;
use pine_richtext::view::{Editor as RichTextHandle, Markdown, PineRichTextRoot};
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

    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = refs.get("root") else {
            return;
        };
        let handle = this::<Editor>();
        install_task_toggle_listener(root, handle);
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
        if let Some(editor) = self.editor_handle() {
            if let Err(err) = editor.set::<Markdown>(&md) {
                self.exported_markdown = format!("(import error: {err})");
            }
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

    /// Flip the `checked` attribute on the `task_item` at `pos`. Called
    /// from the `pine:task-toggle` custom event dispatched by the
    /// `<pine-task-item>` node-view component.
    fn toggle_task_checked(&mut self, pos: usize, checked: bool) {
        self.with_editor(|e| {
            e.dispatch(CommandRequest::SetNodeAttr {
                pos,
                attr: "checked".into(),
                value: json!(checked),
            })
        });
    }
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

/// Bench config parsed out of `window.location.search`. The harness
/// drives the demo via `?bench=large&paragraphs=500&words=80` so a
/// single demo binary supports many doc sizes without recompiling.
///
/// The `shape` axis selects the block layout the seed doc generates
/// — plain paragraphs by default, or task items (custom-element
/// node-views) for the `tasks` preset. Different shapes exercise
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
        // `tasks` seeds many `<pine-task-item>` node-views, which
        // exercise the custom-element render path the plain-paragraph
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
                        schema_basic::text(block_text(i), Vec::new()).unwrap()
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
                    "Bench doc ({} task items × {} words). Type into me to measure custom-element keystroke latency.",
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
                        vec![schema_basic::paragraph(vec![schema_basic::text(
                            block_text(i),
                            Vec::new(),
                        )
                        .unwrap()])
                        .unwrap()],
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
