//! Body editor shared by the Keep composer and note dialog.
//!
//! For `kind == "text"` the editor is a single
//! `<pine-rich-text-root runtime="keep" pp-ref="surface">` —
//! no reactive `pp-model` content binding. The parent
//! [`KeepNoteForm`](crate::components::note_form::KeepNoteForm)
//! reaches a typed handle via RFC 081
//! `refs.body().component::<KeepNoteBody>()` and calls
//! [`KeepNoteBody::editor`] to get a TipTap-style
//! [`pine_richtext::view::Editor`] handle —
//! [`pine_richtext::view::DocNode`] (typed `Node`) for loads /
//! saves so the round-trip is lossless and stays on the
//! typed `Node` shape end-to-end. Markdown stays an export
//! format (clipboard / share).
//!
//! `KeepNote.body_state` is the canonical doc `Node` (wrapped
//! in `Option` for "no doc yet"); `KeepNote.body` is a
//! plain-text preview derived from it on save, used for
//! cards, search, and the `text ↔ checklist` toggle's
//! line-by-line conversion. See [`doc_to_preview_text`] and
//! [`doc_to_title`].
//!
//! Legacy notes saved before the JSON storage switch have a
//! `None` `body_state` and a markdown `body` — the load path
//! detects that and falls back to `editor.set::<Markdown>(body)`
//! so existing rows still open; their next save migrates to a
//! `Some(node)` automatically.
//!
//! See [`crate::richtext_schema::KeepNoteSchemaExtension`] for
//! the schema that puts a `title` node at the top.

use pine_richtext::model::Node as RichTextNode;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::KeepTodo;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "KeepNoteBody.poco",
    style = "KeepNoteBody.css",
    role = "panel",
    display = "contents"
)]
pub struct KeepNoteBody {
    #[prop]
    pub kind: String,
    #[model]
    pub todos: Vec<KeepTodo>,
    #[model]
    pub todo_text: String,
    pub next_todo_id: u64,
    /// This component's own [`ScopeId`](pocopine::ScopeId) as
    /// a raw `u64`. Captured at `on_setup` so
    /// [`Self::editor`] can resolve the
    /// `pp-ref="surface"` Element under THIS scope when
    /// invoked from a parent's handler via
    /// `body.with(|b| b.editor())` — `Handle::with` doesn't
    /// rebind `current_scope_id`, so without this stash the
    /// ref lookup would query the parent's scope by mistake.
    /// `u64` rather than `ScopeId` keeps the field
    /// `Serialize + Deserialize` for the component macro's
    /// proxy round-trip.
    pub scope_id_seed: u64,
}

#[handlers]
impl KeepNoteBody {
    pub fn on_setup(&mut self) {
        if let Some(id) = pocopine::current_scope_id() {
            self.scope_id_seed = id.0;
        }
    }

    pub fn add_todo(&mut self) {
        let text = self.todo_text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.push_todo(text, false);
        self.todo_text.clear();
    }

    pub fn remove_todo(&mut self, todo_id: String) {
        self.todos.retain(|todo| todo.id != todo_id);
    }

    pub fn toggle_todo(&mut self, todo_id: String) {
        if let Some(todo) = self.todos.iter_mut().find(|todo| todo.id == todo_id) {
            todo.done = !todo.done;
        }
    }
}

impl KeepNoteBody {
    /// RFC 081 — typed accessor for the inner rich-text
    /// surface. Resolves via the `pp-ref="surface"` entry
    /// registered in *this* scope's ref table (captured at
    /// `on_setup`), so it works from any caller's frame
    /// including a parent's `body.with(|b| b.editor())`.
    /// Returns `None` before mount or when `kind ==
    /// "checklist"` (the surface isn't rendered).
    pub fn editor(&self) -> Option<pine_richtext::view::Editor> {
        if self.scope_id_seed == 0 {
            return None;
        }
        let scope = pocopine::ScopeId(self.scope_id_seed);
        let surface = pocopine::refs::get_on(scope, "surface")?;
        Some(pine_richtext::view::Editor::from_element(surface))
    }

    fn push_todo(&mut self, text: String, done: bool) {
        self.next_todo_id = self.next_todo_id.saturating_add(1);
        self.todos.push(KeepTodo {
            id: format!("todo_{}_{}", crate::now_ms(), self.next_todo_id),
            text,
            done,
        });
    }
}

/// Combine a separated `(title, body)` pair into the markdown
/// buffer the rich-text surface expects on the legacy load
/// path. Mirrors the inverse of [`split_title_and_body`]: the
/// title becomes the first `# Title` line; the body follows
/// after a blank line.
///
/// Only used for legacy markdown bodies (notes saved before
/// `body_state` started carrying the JSON doc). New saves write
/// the title-bearing doc into `body_state` directly; the
/// markdown buffer here is the fallback the load path runs
/// through `editor.set::<Markdown>` when `body_state` is empty.
pub(crate) fn combine_title_and_body(title: &str, body: &str) -> String {
    if title.is_empty() && body.is_empty() {
        return String::new();
    }
    // Always emit a leading `# ` even for empty-title notes: the
    // keep schema's `(title | heading) block*` content expression
    // rejects body-only markdown, so without this the legacy
    // `set::<Markdown>` load path silently fails and the next save
    // overwrites the note with an empty surface.
    if body.is_empty() {
        return format!("# {title}");
    }
    format!("# {title}\n\n{body}")
}

/// Walk a doc [`RichTextNode`] and concatenate every text
/// leaf's content. Used to derive `KeepNote.body` (a plain-text
/// preview) from the canonical `KeepNote.body_state` `Node`.
/// The extracted text drives card previews, the search index,
/// and the `text → checklist` toggle's line-by-line split —
/// none of which need markdown syntax noise.
///
/// Blocks are separated by `\n` so the buffer reads like a
/// flattened transcript. The leading title block is included
/// so the `# Title` semantics survive when the preview is
/// displayed standalone; callers that need just the body (no
/// title) can call [`doc_to_title`] to peel it off first.
///
/// Returns `None` when the node isn't a `doc` — caller should
/// treat the legacy markdown buffer as the seed in that case.
pub(crate) fn doc_to_preview_text(doc: &RichTextNode) -> Option<String> {
    if doc.type_name() != "doc" {
        return None;
    }
    let mut out = String::new();
    for (i, child) in doc.content().iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        append_block_text(child, &mut out);
    }
    Some(out)
}

/// Extract the first-child block's text. The keep schema puts
/// a `title` (or `heading` post markdown re-import) at
/// position 0; this returns its inline text. Used at save time
/// to keep `KeepNote.title` in sync with whatever the user
/// typed into the title slot.
///
/// Attrs stay [`Value`]-backed in the model (they're
/// schema-dynamic per node type — `level` for headings,
/// `checked` for task-items, etc.) so the `heading` level
/// lookup still goes through `Value::as_u64`. Everything else
/// is typed.
pub(crate) fn doc_to_title(doc: &RichTextNode) -> Option<String> {
    if doc.type_name() != "doc" {
        return None;
    }
    let first = doc.content().as_slice().first()?;
    let ty = first.type_name();
    let is_title_slot = ty == "title"
        || (ty == "heading" && first.attrs().get("level").and_then(Value::as_u64) == Some(1));
    if !is_title_slot {
        return None;
    }
    let mut out = String::new();
    for child in first.content().iter() {
        append_block_text(child, &mut out);
    }
    Some(out)
}

/// Recursive walker shared by [`doc_to_preview_text`] /
/// [`doc_to_title`]: depth-first through `content`, accumulates
/// every text leaf. Text nodes that carry marks still
/// contribute the raw `text` — marks are styling, not content,
/// and previews shouldn't render them.
fn append_block_text(node: &RichTextNode, out: &mut String) {
    if let Some(text) = node.text() {
        out.push_str(text);
        return;
    }
    for child in node.content().iter() {
        append_block_text(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a `Node` from inline JSON for compact test setup.
    /// The model's `Deserialize` impl is the same one that
    /// drives the editor's load path, so any shape that round-
    /// trips here also round-trips through `set::<DocNode>`.
    fn doc_from_json(v: Value) -> RichTextNode {
        serde_json::from_value(v).expect("valid node json")
    }

    #[test]
    fn combine_emits_canonical_markdown_envelope() {
        assert_eq!(combine_title_and_body("Title", "Body."), "# Title\n\nBody.");
        // Empty-title legacy notes still need a leading heading
        // because keep's `(title | heading) block*` content
        // expression rejects body-only markdown. The empty `# `
        // becomes a heading-1 with no text — the load path
        // round-trips it as an empty title slot.
        assert_eq!(combine_title_and_body("", "Body only."), "# \n\nBody only.");
        assert_eq!(combine_title_and_body("Title only", ""), "# Title only");
        assert_eq!(combine_title_and_body("", ""), "");
    }

    #[test]
    fn doc_preview_extracts_text_across_blocks() {
        // A title + two paragraph blocks. Preview joins each
        // top-level block's text with a single newline. Marks
        // don't appear (they're styling, not content).
        let doc = doc_from_json(json!({
            "type": "doc",
            "content": [
                {"type": "title", "content": [{"type": "text", "text": "My title"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "First."}]},
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "Some "},
                    {"type": "text", "marks": [{"type": "strong"}], "text": "bold"},
                    {"type": "text", "text": " text."}
                ]}
            ]
        }));
        assert_eq!(
            doc_to_preview_text(&doc).unwrap(),
            "My title\nFirst.\nSome bold text."
        );
    }

    #[test]
    fn doc_preview_returns_none_when_root_is_not_a_doc() {
        let para = doc_from_json(json!({"type": "paragraph", "content": []}));
        assert!(doc_to_preview_text(&para).is_none());
    }

    #[test]
    fn doc_title_extracts_title_node_text() {
        let doc = doc_from_json(json!({
            "type": "doc",
            "content": [
                {"type": "title", "content": [{"type": "text", "text": "Hello title"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "Body."}]}
            ]
        }));
        assert_eq!(doc_to_title(&doc).as_deref(), Some("Hello title"));
    }

    #[test]
    fn doc_title_also_matches_leading_heading_level_1() {
        // After a markdown round-trip the leading slot is a
        // `heading` with `level=1`, not a `title` node. Both
        // shape-match for title extraction.
        let doc = doc_from_json(json!({
            "type": "doc",
            "content": [
                {"type": "heading", "attrs": {"level": 1},
                 "content": [{"type": "text", "text": "Reload title"}]}
            ]
        }));
        assert_eq!(doc_to_title(&doc).as_deref(), Some("Reload title"));
    }

    #[test]
    fn doc_title_skips_non_title_leading_block() {
        let doc = doc_from_json(json!({
            "type": "doc",
            "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "Just body."}]}
            ]
        }));
        assert!(doc_to_title(&doc).is_none());
    }
}
