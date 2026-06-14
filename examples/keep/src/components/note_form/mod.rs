//! Shared note editor used by the inline composer and modal editor.
//!
//! The component owns one local edit buffer. Shells provide context
//! (`draft` vs `editor`) and the form dispatches either a create or update
//! when the buffer is saved.
//!
//! The rich-text body editor (when `kind == "text"`) is reached
//! through the RFC 081 typed-refs API. The template tags
//! `<keep-note-body pp-ref="body">`; `KeepNoteFormRefs.body()`
//! gives a typed accessor, `.component::<KeepNoteBody>()`
//! resolves the child handle, and
//! [`KeepNoteBody::editor`](crate::components::note_body::KeepNoteBody::editor)
//! returns the [`pine_richtext::view::Editor`] surface handle.
//! No DOM drilling, no thread-local cache — the typed handle
//! is captured once in `on_ready` and cloned into store-watcher
//! closures for tick::next continuations.
//!
//! - **Load** (via [`KeepNoteForm::push_body_to_surface`]):
//!   `editor.set::<DocNode>(node)` is called whenever new
//!   content arrives from the store, with
//!   `editor.set::<Markdown>(buffer)` reserved as the fallback
//!   for legacy notes saved before the JSON migration. Silent
//!   state swap — does NOT fire the surface's doc-changed
//!   event, so it can't feedback-loop against any parent
//!   watcher.
//! - **Save**: [`KeepNoteForm::save`] calls
//!   `editor.get::<DocNode>()` at the exact moment of save and
//!   derives `(title, body_preview)` from that typed node — no
//!   lagging state mirror, no markdown intermediate.
//!
//! See [`pine_richtext::view::Editor`] for the full handle API
//! (`set`/`get`/`on_update` over any [`pine_richtext::view::ContentFormat`],
//! `dispatch`, `toggle_mark`, `undo`/`redo`, …).

use pocopine::create_context;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::KeepTodo;
use crate::components::note_body::KeepNoteBody;
use crate::store::{
    KeepEditorData, KeepFormNote, KeepLabelOption, KeepStore, KeepViewMode, can_create_label,
    format_todo_line, label_picker_options_for, parse_todo_line,
};
use pine_richtext::model::Node as RichTextNode;
use pine_richtext::view::{DocNode, Markdown};

create_context!(pub(crate) KEEP_NOTE_FORM_CONTEXT: KeepNoteFormContext);

#[derive(Clone, Copy)]
pub(crate) struct KeepNoteFormContext {
    pub mode: &'static str,
}

impl KeepNoteFormContext {
    pub const DRAFT_COMPOSER: Self = Self { mode: "draft" };

    pub const EDITOR_MODAL: Self = Self { mode: "editor" };
}

#[derive(Default, Serialize, Deserialize)]
#[component(style = "KeepNoteForm.css")]
pub struct KeepNoteForm {
    pub mode: String,
    pub note_id: String,
    pub title: String,
    /// Plain-text body buffer. For text notes this is the
    /// preview text derived from the doc on save; for checklist
    /// notes it's the pre-split body before the `\n` → todos
    /// conversion. Card / search / preview surfaces read it
    /// directly.
    pub body: String,
    /// Doc state [`RichTextNode`] for text notes. Acts as the
    /// seed for `editor.set::<DocNode>(node)` on load and gets
    /// refilled from `editor.get::<DocNode>()` on save. `None`
    /// for checklist notes and for fresh drafts before the first
    /// save.
    pub body_state: Option<RichTextNode>,
    pub kind: String,
    pub color: String,
    pub todos: Vec<KeepTodo>,
    pub labels: Vec<String>,
    pub pinned: bool,
    pub todo_text: String,
    pub label_picker_open: bool,
    pub label_query: String,
    pub label_options: Vec<KeepLabelOption>,
    pub label_can_create: bool,
    pub color_picker_open: bool,
    pub next_todo_id: u64,
}

#[handlers]
impl KeepNoteForm {
    pub fn on_setup(&mut self) {
        let ctx = KEEP_NOTE_FORM_CONTEXT
            .inject()
            .unwrap_or(KeepNoteFormContext::DRAFT_COMPOSER);
        self.mode = ctx.mode.into();
        self.reset_draft("text");

        // Synchronous load for the editor-mode mount-after-set case
        // (list-detail's `cycle_view_mode` auto-opens the first
        // visible note *before* this form mounts). Without this the
        // first paint shows blank inputs because the editor_open /
        // editor_data watchers in `on_ready` are registered after
        // those store fields have already settled — they miss the
        // transition entirely. Reading the store via `with(...)` is
        // a read-only borrow that's safe inside `on_setup`.
        if self.mode == "editor" {
            self.load_editor_from_store();
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: KeepNoteFormRefs) {
        // RFC 081 — resolve the child component's typed handle
        // once, while scope is live. The handle is cheap to
        // clone (just `Rc<RefCell<T>>` + scope id) and stable
        // for the form's lifetime; every store-watcher closure
        // clones it before deferring into `tick::next`.
        let body: Option<pocopine::Handle<KeepNoteBody>> = refs.body().component::<KeepNoteBody>();

        // Seeding the surface from `on_setup`'s pre-loaded
        // fields used to happen here. It now goes through the
        // canonical `schedule_load_editor` path below — that
        // helper splits the load and the push across two
        // `tick::next` calls, which is what gets list-detail
        // working: the layout, form, body, and inner
        // `<pine-rich-text-root>` all mount inside one
        // reactive flush, and the surface's command-event
        // listener isn't installed until *after* that flush
        // settles. A push scheduled in the same tick as the
        // mount silently drops because no listener is attached
        // yet. The two-tick wait makes the dispatch land.

        if self.mode == "draft" {
            let h = handle.clone();
            let body_for_open = body.clone();
            pocopine::store::<KeepStore>().watch_field::<bool, _>(
                "composer_open",
                move |open, prev| {
                    let was_open = prev.copied().unwrap_or(false);
                    if *open && !was_open {
                        schedule_prepare_draft(h.clone(), body_for_open.clone());
                    } else if !*open && was_open {
                        schedule_reset_draft(h.clone(), body_for_open.clone());
                    }
                },
            );

            let h = handle.clone();
            let body_for_kind = body.clone();
            pocopine::store::<KeepStore>().watch_field::<String, _>("draft_kind", move |_, _| {
                schedule_prepare_draft(h.clone(), body_for_kind.clone());
            });
        } else {
            let h = handle.clone();
            let body_for_open = body.clone();
            pocopine::store::<KeepStore>().watch_field::<bool, _>(
                "editor_open",
                move |open, prev| {
                    let was_open = prev.copied().unwrap_or(false);
                    if *open && !was_open {
                        schedule_load_editor(h.clone(), body_for_open.clone());
                    }
                },
            );

            let h = handle.clone();
            let body_for_data = body.clone();
            pocopine::store::<KeepStore>().watch_field::<KeepEditorData, _>(
                "editor_data",
                move |data, prev| {
                    let previous = prev.cloned().unwrap_or_default();
                    if data.id != previous.id {
                        // List-detail row switch: persist the form's
                        // local edits against the previous note before
                        // overwriting them with the new row's data.
                        let is_list = pocopine::store::<KeepStore>()
                            .with(|s| s.view_mode == KeepViewMode::List);
                        if is_list && !previous.id.is_empty() {
                            schedule_save_then_load(h.clone(), body_for_data.clone());
                        } else {
                            schedule_load_editor(h.clone(), body_for_data.clone());
                        }
                    } else if data.pinned != previous.pinned {
                        schedule_sync_editor_pin(h.clone());
                    }
                },
            );

            // Late-mount case: when entering list-detail mode the
            // store auto-opens the first visible note *before* this
            // form mounts, so the watchers above are registered
            // after the false→true editor_open transition has
            // already happened. Trigger an explicit load so the
            // right pane shows the active note on first render.
            // The helper is a no-op when editor_open is false.
            schedule_load_editor(handle.clone(), body.clone());
        }

        let picker = handle.clone();
        handle.watch_field::<bool, _>("label_picker_open", move |open, _| {
            if *open {
                picker.update(KeepNoteForm::rebuild_label_options);
            }
        });

        let picker = handle.clone();
        handle.watch_field::<String, _>("label_query", move |_, _| {
            picker.update(KeepNoteForm::rebuild_label_options);
        });
    }

    pub fn save(&mut self) {
        if !self.is_active() {
            return;
        }
        self.close_popovers();
        let form = self.collect_form_from_live_surface();
        if self.mode == "editor" {
            crate::shared_layout_transition(move |s| {
                s.save_form_note(form);
            });
        } else {
            pocopine::store::<KeepStore>().update(move |s| {
                s.save_form_note(form);
            });
        }
    }

    /// Click-outside auto-save. In the list-detail pane the right
    /// pane is permanently mounted, so every click on the topbar,
    /// sidebar, or list rows would otherwise generate a no-op
    /// upsert. List mode auto-saves on row switch instead (handled
    /// by the editor_data id watcher in `on_ready`).
    pub fn auto_save(&mut self) {
        if pocopine::store::<KeepStore>().with(|s| s.view_mode == KeepViewMode::List) {
            return;
        }
        self.save();
    }

    pub fn cancel(&mut self) {
        self.close_popovers();
        if self.mode == "editor" {
            crate::shared_layout_transition(KeepStore::cancel_editor);
        } else {
            self.reset_draft("text");
            pocopine::store::<KeepStore>().update(KeepStore::cancel_composer);
        }
    }

    pub fn toggle_kind(&mut self) {
        let body = pocopine::refs::get_component::<KeepNoteBody>("body");
        if self.kind == "checklist" {
            let mut lines: Vec<String> = self.todos.iter().map(format_todo_line).collect();
            if !self.body.is_empty() {
                lines.insert(0, self.body.clone());
            }
            self.body = lines.join("\n");
            // Reset the JSON state so the surface seeds from the
            // joined-lines markdown via the fallback path. The
            // next save promotes the surface's doc back into a
            // fresh `body_state`.
            self.body_state = None;
            self.todos.clear();
            self.kind = "text".into();
            // The text surface mounts on the next reactive
            // flush (pp-if reveals it once `kind == "text"`
            // commits). Push the seeded body into the new
            // surface after it mounts.
            let handle = this::<Self>();
            pocopine::tick::next(move || {
                handle.update(move |form| form.push_body_to_surface(body.as_ref()));
            });
        } else {
            // Pull the live body out of the surface BEFORE
            // it unmounts so toggle_kind parses what the
            // user typed, not the stale seed value. Markdown
            // (not the JSON doc) is what splits cleanly on
            // `\n` into todo lines, so the read path stays on
            // `editor.get::<Markdown>()`.
            if let Some(latest) = body.and_then(read_surface_markdown) {
                self.body = latest;
            }
            let lines: Vec<String> = self.body.lines().map(str::to_string).collect();
            for line in lines {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let (text, done) = parse_todo_line(trimmed);
                self.push_todo(text, done);
            }
            self.body.clear();
            self.body_state = None;
            self.kind = "checklist".into();
        }
    }

    pub fn pick_color(&mut self, color: String) {
        self.color_picker_open = false;
        self.color = color.clone();
        if self.mode == "editor" {
            pocopine::store::<KeepStore>().update(move |s| s.set_editor_color(color));
        } else {
            pocopine::store::<KeepStore>().update(move |s| s.set_draft_color(color));
        }
    }

    pub fn archive(&mut self) {
        if self.mode != "editor" || !self.is_active() {
            return;
        }
        self.close_popovers();
        let form = self.collect_form_from_live_surface();
        crate::shared_layout_transition(move |s| {
            s.archive_form_note(form);
        });
    }

    /// Snapshot the form into a save payload, preferring the live
    /// rich-text surface for text notes over the bound fields.
    /// `save()` and `archive()` both rely on this so neither path
    /// can persist a stale seed and drop in-progress edits — the
    /// text-note template removed the `pp-model` title/body inputs,
    /// so `self.title` / `self.body` / `self.body_state` are only
    /// current if a save has already flushed them.
    fn collect_form_from_live_surface(&mut self) -> KeepFormNote {
        if self.kind != "text" {
            return self.collect_fields();
        }
        let read =
            pocopine::refs::get_component::<KeepNoteBody>("body").and_then(read_surface_doc_state);
        if let Some(body_state) = read {
            let title = crate::components::note_body::doc_to_title(&body_state).unwrap_or_default();
            let body = crate::components::note_body::doc_to_preview_text(&body_state)
                .map(|combined| strip_leading_title(&combined, &title))
                .unwrap_or_default();
            self.collect_text_fields(title, body, Some(body_state))
        } else {
            // Surface wasn't available — fall back to the
            // last-loaded buffers so the save doesn't clobber
            // persisted content with empty values.
            let title = self.title.clone();
            let body = self.body.clone();
            let body_state = self.body_state.clone();
            self.collect_text_fields(title, body, body_state)
        }
    }

    pub fn delete(&mut self) {
        if self.mode != "editor" || self.note_id.is_empty() {
            return;
        }
        self.close_popovers();
        let id = self.note_id.clone();
        crate::shared_layout_transition(move |s| {
            s.delete_note(id);
            // Drop the editor: the row is gone from `notes.rows`,
            // so any list-detail / modal pane should fall back to
            // the empty placeholder.
            s.cancel_editor();
        });
    }

    pub fn toggle_label(&mut self, label: String) {
        if let Some(pos) = self.labels.iter().position(|existing| existing == &label) {
            self.labels.remove(pos);
        } else {
            self.labels.push(label.clone());
            pocopine::store::<KeepStore>().update(move |s| s.add_label(label));
        }
        self.rebuild_label_options();
    }

    pub fn create_label(&mut self) {
        let label = self.label_query.trim().to_string();
        if label.is_empty() {
            return;
        }
        let labels = pocopine::store::<KeepStore>().with(|s| s.labels.clone());
        if !can_create_label(&labels, &label) {
            return;
        }
        self.label_query.clear();
        if !self.labels.iter().any(|existing| existing == &label) {
            self.labels.push(label.clone());
        }
        self.select_label_option(&label);
        pocopine::store::<KeepStore>().update(move |s| s.add_label(label));
    }
}

fn schedule_prepare_draft(
    handle: pocopine::Handle<KeepNoteForm>,
    body: Option<pocopine::Handle<KeepNoteBody>>,
) {
    // Single tick: reset fields + push (via `Editor::clear`
    // when the buffers are empty) in the same handler.
    pocopine::tick::next(move || {
        handle.update(move |form| {
            form.prepare_draft_from_store();
            form.push_body_to_surface(body.as_ref());
        });
    });
}

fn schedule_reset_draft(
    handle: pocopine::Handle<KeepNoteForm>,
    body: Option<pocopine::Handle<KeepNoteBody>>,
) {
    // Single tick: reset fields + push (clears surface). The
    // composer collapses after a save; the push here is what
    // ensures the contenteditable doesn't keep the just-saved
    // note's content visible when the composer re-expands.
    pocopine::tick::next(move || {
        handle.update(move |form| {
            form.reset_draft("text");
            form.push_body_to_surface(body.as_ref());
        });
    });
}

fn schedule_load_editor(
    handle: pocopine::Handle<KeepNoteForm>,
    body: Option<pocopine::Handle<KeepNoteBody>>,
) {
    // Single tick: load fields + push to surface in the same
    // handler. The surface's `Editor::set` / `clear` are
    // ready-aware now — if the surface's command listener
    // hasn't been installed yet (component mid-mount), the
    // dispatch is queued on `requestAnimationFrame` and
    // replays once `data-pine-richtext-ready="true"` lands.
    // No more two-tick guessing.
    pocopine::tick::next(move || {
        handle.update(move |form| {
            form.load_editor_from_store();
            form.push_body_to_surface(body.as_ref());
        });
    });
}

fn schedule_save_then_load(
    handle: pocopine::Handle<KeepNoteForm>,
    body: Option<pocopine::Handle<KeepNoteBody>>,
) {
    // Single tick. Editor handles its own readiness, so the
    // save + load + push all run in one handler without
    // racing the surface's mount.
    pocopine::tick::next(move || {
        handle.update(move |form| {
            // Pull the live doc state out of the surface so the
            // about-to-be-replaced row gets saved with the
            // user's typed edits, not the stale snapshot
            // `self.body_state` was seeded with on load. Read
            // the typed Node (canonical) and derive
            // `title` / `body` from it; markdown stays an
            // export-only format.
            if form.kind == "text"
                && let Some(state) = body.clone().and_then(read_surface_doc_state)
            {
                form.title = crate::components::note_body::doc_to_title(&state)
                    .unwrap_or_else(|| form.title.clone());
                form.body = crate::components::note_body::doc_to_preview_text(&state)
                    .map(|combined| strip_leading_title(&combined, &form.title))
                    .unwrap_or_default();
                form.body_state = Some(state);
            }
            let snapshot = form.collect_fields();
            if !snapshot.note_id.is_empty() {
                pocopine::store::<KeepStore>().update(move |s| s.save_form_note(snapshot));
            }
            form.load_editor_from_store();
            form.push_body_to_surface(body.as_ref());
        });
    });
}

/// RFC 081 — read the current markdown out of the
/// `<keep-note-body>` child's rich-text surface, given a
/// typed handle. Returns `None` when the surface isn't
/// mounted (checklist mode), the body hasn't initialized
/// yet, or the markdown export pipeline rejected the state.
///
/// Markdown is the export-only format now (clipboard / share);
/// `read_surface_doc_state` is the canonical read for saves.
fn read_surface_markdown(body: pocopine::Handle<KeepNoteBody>) -> Option<String> {
    body.with(|b| b.editor()?.get::<Markdown>().ok())
}

/// Read the current doc state as a typed [`RichTextNode`].
/// Used by the save path so reloads round-trip through
/// `editor.set::<DocNode>(&node)` directly — no `Value`
/// intermediate, no markdown serializer.
fn read_surface_doc_state(body: pocopine::Handle<KeepNoteBody>) -> Option<RichTextNode> {
    body.with(|b| b.editor()?.get::<DocNode>().ok())
}

/// Trim the leading title block's text (followed by its `\n`
/// separator) off a doc preview. The preview helper joins every
/// top-level block with `\n`; the save path persists title and
/// body separately, so the body preview shouldn't double up the
/// title text.
fn strip_leading_title(preview: &str, title: &str) -> String {
    if title.is_empty() {
        return preview.to_string();
    }
    let candidate = match preview.split_once('\n') {
        Some((first, rest)) if first == title => rest,
        _ => preview,
    };
    candidate.to_string()
}

fn schedule_sync_editor_pin(handle: pocopine::Handle<KeepNoteForm>) {
    pocopine::tick::next(move || {
        handle.update(KeepNoteForm::sync_editor_pin);
    });
}

impl KeepNoteForm {
    fn close_popovers(&mut self) {
        self.color_picker_open = false;
        self.label_picker_open = false;
    }

    fn is_active(&self) -> bool {
        let mode = self.mode.clone();
        pocopine::store::<KeepStore>().with(|s| {
            if mode == "editor" {
                s.editor_open
            } else {
                s.composer_open
            }
        })
    }

    fn prepare_draft_from_store(&mut self) {
        if self.mode != "draft" {
            return;
        }
        let Some(kind) = pocopine::store::<KeepStore>().with(|s| {
            s.composer_open.then(|| {
                if s.draft_kind == "checklist" {
                    "checklist".to_string()
                } else {
                    "text".to_string()
                }
            })
        }) else {
            return;
        };
        self.reset_draft(&kind);
    }

    fn reset_draft(&mut self, kind: &str) {
        self.note_id.clear();
        self.title.clear();
        self.body.clear();
        self.body_state = None;
        self.kind = if kind == "checklist" {
            "checklist".into()
        } else {
            "text".into()
        };
        self.color = "default".into();
        self.todos.clear();
        self.labels.clear();
        self.pinned = false;
        self.todo_text.clear();
        self.label_query.clear();
        self.label_options.clear();
        self.label_can_create = false;
        self.close_popovers();
    }

    fn load_editor_from_store(&mut self) {
        if self.mode != "editor" {
            return;
        }
        let Some(data) =
            pocopine::store::<KeepStore>().with(|s| s.editor_open.then(|| s.editor_data.clone()))
        else {
            return;
        };
        self.note_id = data.id;
        self.kind = if data.kind == "checklist" {
            "checklist".into()
        } else {
            "text".into()
        };
        // Text notes prefer the lossless JSON doc state. Legacy
        // rows saved before this format lived only carry a
        // markdown `body` — those flow through the same
        // `push_body_to_surface` path which detects the
        // representation and chooses `set::<Doc>` vs
        // `set::<Markdown>`.
        if data.kind == "checklist" {
            self.title = data.title;
            self.body = data.body;
            self.body_state = None;
        } else {
            self.title = data.title.clone();
            // The markdown-fallback seed is the combined
            // `# Title\n\nBody` shape; `push_body_to_surface`
            // only reads it when `body_state` is `None` (legacy
            // rows saved before the JSON migration).
            self.body = if data.body_state.is_none() {
                crate::components::note_body::combine_title_and_body(&data.title, &data.body)
            } else {
                data.body
            };
            self.body_state = data.body_state;
        }
        self.color = if data.color.is_empty() {
            "default".into()
        } else {
            data.color
        };
        self.todos = data.todos;
        self.labels = data.labels;
        self.pinned = data.pinned;
        self.todo_text.clear();
        self.label_query.clear();
        self.rebuild_label_options();
        self.close_popovers();
    }

    fn sync_editor_pin(&mut self) {
        if self.mode != "editor" {
            return;
        }
        if let Some(pinned) =
            pocopine::store::<KeepStore>().with(|s| s.editor_open.then_some(s.editor_data.pinned))
        {
            self.pinned = pinned;
        }
    }

    fn collect_fields(&mut self) -> KeepFormNote {
        let title = self.title.clone();
        let body = self.body.clone();
        let body_state = self.body_state.clone();
        self.collect_text_fields(title, body, body_state)
    }

    /// Build a save payload from explicit `(title, body,
    /// body_state)` values instead of the corresponding `self`
    /// fields. The text-note save path derives all three from
    /// the surface's current doc JSON without touching the
    /// bound fields; checklist saves pass `self.body` /
    /// `self.body_state` (the latter empty).
    fn collect_text_fields(
        &mut self,
        title: String,
        body: String,
        body_state: Option<RichTextNode>,
    ) -> KeepFormNote {
        let mut todos = self.todos.clone();
        let inline = self.todo_text.trim().to_string();
        if self.kind == "checklist" && !inline.is_empty() {
            self.next_todo_id = self.next_todo_id.saturating_add(1);
            todos.push(KeepTodo {
                id: format!("todo_{}_{}", crate::now_ms(), self.next_todo_id),
                text: inline,
                done: false,
            });
            self.todo_text.clear();
        }
        KeepFormNote {
            note_id: self.note_id.clone(),
            title,
            body,
            body_state,
            color: self.color.clone(),
            todos,
            labels: self.labels.clone(),
            pinned: self.pinned,
        }
    }

    fn push_todo(&mut self, text: String, done: bool) {
        self.next_todo_id = self.next_todo_id.saturating_add(1);
        self.todos.push(KeepTodo {
            id: format!("todo_{}_{}", crate::now_ms(), self.next_todo_id),
            text,
            done,
        });
    }

    fn rebuild_label_options(&mut self) {
        let labels = pocopine::store::<KeepStore>().with(|s| s.labels.clone());
        let (options, can_create) =
            label_picker_options_for(&labels, &self.labels, &self.label_query);
        self.label_options = options;
        self.label_can_create = can_create;
    }

    fn select_label_option(&mut self, label: &str) {
        if let Some(option) = self
            .label_options
            .iter_mut()
            .find(|option| option.name == label)
        {
            option.selected = true;
        } else {
            self.label_options.push(KeepLabelOption {
                name: label.to_string(),
                selected: true,
                visible: true,
            });
        }
    }

    /// Push the form's current body buffer into the mounted
    /// surface via the child's [`KeepNoteBody::editor`] helper.
    /// Silent no-op when the typed handle is `None`, the
    /// surface isn't in the DOM (checklist mode), or the
    /// content parser rejected the input.
    ///
    /// Counterpart to TipTap's
    /// `editor.commands.setContent(html, { emitUpdate: false })`
    /// — replaces content without firing the doc-changed
    /// listeners, so the parent can call it from inside its
    /// own watchers without recursion.
    ///
    /// Prefers the JSON doc state in `self.body_state` (the
    /// canonical storage form) and falls back to parsing
    /// `self.body` as markdown — that path keeps legacy notes
    /// saved before the JSON migration loadable.
    pub(crate) fn push_body_to_surface(&self, body: Option<&pocopine::Handle<KeepNoteBody>>) {
        if self.kind != "text" {
            return;
        }
        let Some(body) = body else {
            return;
        };
        let body_state = self.body_state.clone();
        let body_markdown = self.body.clone();
        body.with(|b| {
            let Some(editor) = b.editor() else { return };
            // Three load paths, in priority order. Each call
            // is fire-and-forget against the surface's
            // ready-aware dispatch — if the surface's command
            // listener isn't wired yet (mid-mount), the
            // request is queued on `requestAnimationFrame` and
            // replays on subsequent frames until ready.
            // Callers don't need to time anything.
            if let Some(node) = &body_state {
                let _ = editor.set::<DocNode>(node);
                return;
            }
            if !body_markdown.is_empty() {
                let _ = editor.set::<Markdown>(&body_markdown);
                return;
            }
            // Fresh draft (no doc, no markdown) — clear the
            // surface to the runtime's empty default doc, the
            // textarea-equivalent of `.value = ""`. Robust:
            // doesn't go through the markdown parser, doesn't
            // depend on the schema accepting an empty buffer.
            let _ = editor.clear();
        });
    }
}
