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

use pine_richtext::extension;
use pine_richtext::extensions::TaskListExtension;
use pine_richtext::history::history_plugin;
use pine_richtext::model::Attrs;
use pine_richtext::schema_basic;
use pine_richtext::state::{EditorState, EditorStateConfig, Plugin, Selection};
use pine_richtext::view::root::{CommandRequest, COMMAND_EVENT};
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
        install_task_toggle_listener(root, handle);
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

/// Find the editor surface element in the DOM. We assume there's only
/// one `<pine-rich-text-root>` on the page for the demo. Real apps that
/// host multiple editors would pass an explicit reference.
fn find_surface() -> Option<Element> {
    let window = web_sys::window()?;
    let document = window.document()?;
    document
        .query_selector("pine-rich-text-root .pine-rich-text")
        .ok()
        .flatten()
        .or_else(|| {
            document
                .query_selector("pine-rich-text-root")
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

#[wasm_bindgen(start)]
pub fn main() {
    // Phase 4 — extension contract demo. `TaskListExtension::with_node_view`
    // forwards `PineTaskItem`'s tag and content selector into
    // `crate::render::node_views` at registration time, so the
    // reconciler sees the binding before the schema is folded.
    extension::register(Box::new(
        TaskListExtension::new().with_node_view::<PineTaskItem>(),
    ));
    App::new()
        .register::<Editor>()
        .register::<PineRichTextRoot>()
        .register::<PineTaskItem>()
        .run();
}

#[allow(dead_code)]
fn _force_unused_selection_type(_: Selection) {}
