//! `KeepStore` — singleton owning durable Keep app state.
//!
//! Board state, synced rows, labels, command state, and persistence
//! hooks live here. The active note form keeps its transient edit
//! buffer locally and submits a typed save payload back to this store.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{KeepNote, KeepTag};

mod actions;
mod derived;
mod labels;
mod mutations;
mod theme;
mod view;
mod view_mode;

pub use labels::{KeepLabelOption, can_create_label, label_options_for, label_picker_options_for};
pub use theme::KeepTheme;
use theme::load_theme_preference;
pub use view::{KeepCommandNote, KeepEditorData, KeepNoteCardRow};
pub(crate) use view::{KeepFormNote, format_todo_line, parse_todo_line};
pub use view_mode::KeepViewMode;
use view_mode::load_view_mode_preference;

struct CachedAuth {
    display_name: String,
    email: String,
    photo_url: String,
}

#[derive(Serialize, Deserialize)]
#[store(name = "keep")]
pub struct KeepStore {
    pub notes: Vec<KeepNote>,
    pub tags: Vec<KeepTag>,
    pub pinned_notes: Vec<KeepNoteCardRow>,
    pub other_notes: Vec<KeepNoteCardRow>,
    pub command_notes: Vec<KeepCommandNote>,

    pub composer_open: bool,
    pub draft_kind: String,
    pub draft_color: String,

    pub editor_open: bool,
    pub editor_data: KeepEditorData,

    pub sidebar_expanded: bool,
    pub section_kind: String,
    /// When `section_kind == "label"`, the active label name to
    /// filter by. Empty otherwise.
    pub section_label: String,
    pub search_query: String,
    pub command_label_query: String,
    pub theme: KeepTheme,
    pub view_mode: KeepViewMode,
    pub labels: Vec<String>,
    pub selected_note_ids: Vec<String>,
    pub selection_label: String,

    pub auth_ready: bool,
    pub auth_signed_in: bool,
    pub auth_display_name: String,
    pub auth_email: String,
    pub auth_photo_url: String,
    pub auth_initial: String,

    pub status: String,
    pub next_local_id: u64,
    pub resetting: bool,
}

impl Default for KeepStore {
    fn default() -> Self {
        // Hand-rolled so that String fields with semantic defaults
        // ("notes", "text", "default") are correct at the very first
        // template walk — before any handler has a chance to run.
        // on_mount fires *after* the initial walk, so doing this in a
        // handler leaves the first render with section_kind = "" and
        // the PINNED/OTHERS filters evaluating to false.
        let cached_auth = load_cached_auth_snapshot();
        let auth_signed_in = cached_auth.is_some();
        let auth_display_name = cached_auth
            .as_ref()
            .map(|auth| auth.display_name.clone())
            .unwrap_or_default();
        let auth_email = cached_auth
            .as_ref()
            .map(|auth| auth.email.clone())
            .unwrap_or_default();
        let auth_photo_url = cached_auth
            .as_ref()
            .map(|auth| auth.photo_url.clone())
            .unwrap_or_default();
        let auth_initial = actions::auth_initial(&auth_display_name, &auth_email);

        Self {
            notes: Vec::new(),
            tags: Vec::new(),
            pinned_notes: Vec::new(),
            other_notes: Vec::new(),
            command_notes: Vec::new(),

            composer_open: false,
            draft_kind: "text".to_string(),
            draft_color: "default".to_string(),

            editor_open: false,
            editor_data: KeepEditorData::default(),

            sidebar_expanded: false,
            section_kind: "notes".to_string(),
            section_label: String::new(),
            search_query: String::new(),
            command_label_query: String::new(),
            theme: load_theme_preference(),
            view_mode: load_view_mode_preference(),
            labels: Vec::new(),
            selected_note_ids: Vec::new(),
            selection_label: String::new(),

            auth_ready: auth_signed_in,
            auth_signed_in,
            auth_display_name,
            auth_email,
            auth_photo_url,
            auth_initial,

            status: String::new(),
            next_local_id: 0,
            resetting: false,
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn load_cached_auth_snapshot() -> Option<CachedAuth> {
    let session = pocopine::Plugins.get::<pocopine_auth_client::AuthSession>()?;
    if !session.is_authenticated() {
        return None;
    }
    let (display_name, email, photo_url) =
        crate::firebase::keep_auth_fields_from_principal(&session.principal())?;
    Some(CachedAuth {
        display_name,
        email,
        photo_url,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn load_cached_auth_snapshot() -> Option<CachedAuth> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeepTodo;

    fn note(id: &str, pinned: bool, archived: bool, labels: &[&str]) -> KeepNote {
        KeepNote {
            id: id.to_string(),
            title: id.to_string(),
            body: String::new(),
            body_state: None,
            color: "default".to_string(),
            pinned,
            archived,
            todos: Vec::new(),
            labels: labels.iter().map(|label| label.to_string()).collect(),
            updated_at_ms: 0,
        }
    }

    fn push_row(store: &mut KeepStore, note: KeepNote) {
        store.notes.push(note);
    }

    fn row_ids(rows: &[KeepNoteCardRow]) -> Vec<String> {
        rows.iter().map(|row| row.value.id.clone()).collect()
    }

    #[test]
    fn default_store_has_no_placeholder_labels() {
        assert!(KeepStore::default().labels.is_empty());
    }

    #[test]
    fn add_label_trims_and_dedupes() {
        let mut store = KeepStore::default();
        store.add_label(" Work ".to_string());
        store.add_label("Work".to_string());
        store.add_label("   ".to_string());

        assert_eq!(store.labels, vec!["Work"]);
        assert!(store.sidebar_expanded);
    }

    #[test]
    fn command_label_creation_uses_inline_query() {
        let mut store = KeepStore {
            command_label_query: " Ideas ".to_string(),
            ..Default::default()
        };

        store.create_command_label();

        assert_eq!(store.labels, vec!["Ideas"]);
        assert!(store.command_label_query.is_empty());
        assert!(store.sidebar_expanded);
    }

    #[test]
    fn add_label_updates_registry_once() {
        let mut store = KeepStore::default();

        store.add_label("Ideas".to_string());
        store.add_label("Ideas".to_string());

        assert_eq!(store.labels, vec!["Ideas"]);
    }

    #[test]
    fn label_options_mark_selected_labels() {
        let labels = vec!["Work".to_string(), "Ideas".to_string()];
        let selected = vec!["Ideas".to_string()];

        assert_eq!(
            label_options_for(&labels, &selected),
            vec![
                KeepLabelOption {
                    name: "Work".to_string(),
                    selected: false,
                    visible: true,
                },
                KeepLabelOption {
                    name: "Ideas".to_string(),
                    selected: true,
                    visible: true,
                },
            ]
        );
    }

    #[test]
    fn label_picker_options_filter_and_hide_existing_create() {
        let labels = vec!["Work".to_string(), "Ideas".to_string()];
        let selected = vec!["Ideas".to_string()];

        let (options, can_create) = label_picker_options_for(&labels, &selected, "work");
        assert!(!can_create);
        assert_eq!(
            options,
            vec![
                KeepLabelOption {
                    name: "Work".to_string(),
                    selected: false,
                    visible: true,
                },
                KeepLabelOption {
                    name: "Ideas".to_string(),
                    selected: true,
                    visible: false,
                },
            ]
        );

        let (_, can_create) = label_picker_options_for(&labels, &selected, "Travel");
        assert!(can_create);
    }

    #[test]
    fn tag_registry_includes_labels_already_attached_to_notes() {
        let mut store = KeepStore::default();
        push_row(
            &mut store,
            note("labeled", false, false, &["Work", "Ideas"]),
        );
        store.tags.push(KeepTag {
            id: "Travel".to_string(),
            name: "Travel".to_string(),
            updated_at_ms: 0,
        });

        assert_eq!(store.tag_label_registry(), vec!["Travel", "Work", "Ideas"]);
    }

    #[test]
    fn command_notes_include_labels_in_search_value() {
        let mut store = KeepStore::default();
        push_row(
            &mut store,
            KeepNote {
                id: "note-1".to_string(),
                title: "Roadmap".to_string(),
                body: "Polish command palette".to_string(),
                body_state: None,
                color: "default".to_string(),
                pinned: false,
                archived: false,
                todos: vec![KeepTodo {
                    id: "todo-1".to_string(),
                    text: "Ship".to_string(),
                    done: false,
                }],
                labels: vec!["Work".to_string(), "Ideas".to_string()],
                updated_at_ms: 0,
            },
        );

        store.rebuild_visible_notes();

        assert_eq!(store.command_notes.len(), 1);
        assert_eq!(store.command_notes[0].id, "note-1");
        assert!(store.command_notes[0].has_todos);
        assert_eq!(
            store.command_notes[0].search_value,
            "Roadmap Polish command palette Work Ideas"
        );
    }

    #[test]
    fn visible_note_lists_track_active_section() {
        let mut store = KeepStore::default();
        push_row(&mut store, note("pinned", true, false, &[]));
        push_row(&mut store, note("other", false, false, &[]));
        push_row(&mut store, note("archived", false, true, &[]));
        push_row(&mut store, note("work", false, false, &["Work"]));

        store.rebuild_visible_notes();
        assert_eq!(row_ids(&store.pinned_notes), vec!["pinned"]);
        assert_eq!(row_ids(&store.other_notes), vec!["other", "work"]);

        store.show_label("Work".to_string());
        assert!(store.pinned_notes.is_empty());
        assert_eq!(row_ids(&store.other_notes), vec!["work"]);

        store.show_archive();
        assert!(store.pinned_notes.is_empty());
        assert_eq!(row_ids(&store.other_notes), vec!["archived"]);
    }

    #[test]
    fn editor_data_populates_and_resets_as_one_value() {
        let mut store = KeepStore::default();
        push_row(
            &mut store,
            KeepNote {
                id: "editor".to_string(),
                title: "Draft title".to_string(),
                body: String::new(),
                body_state: None,
                color: "sand".to_string(),
                pinned: true,
                archived: false,
                todos: vec![KeepTodo {
                    id: "todo-1".to_string(),
                    text: "Ship it".to_string(),
                    done: false,
                }],
                labels: vec!["Work".to_string()],
                updated_at_ms: 0,
            },
        );

        store.open_editor("editor".to_string());

        assert!(store.editor_open);
        assert_eq!(store.editor_data.id, "editor");
        assert_eq!(store.editor_data.kind, "checklist");
        assert_eq!(store.editor_data.title, "Draft title");
        assert_eq!(store.editor_data.color, "sand");
        assert!(store.editor_data.pinned);
        assert_eq!(store.editor_data.labels, vec!["Work"]);
        assert_eq!(store.editor_data.todos.len(), 1);

        store.cancel_editor();

        assert!(!store.editor_open);
        assert_eq!(store.editor_data, KeepEditorData::default());
    }

    #[test]
    fn selection_toggles_and_formats_count() {
        let mut store = KeepStore::default();
        push_row(&mut store, note("one", false, false, &[]));
        push_row(&mut store, note("two", false, false, &[]));

        store.toggle_note_selection("one".to_string());
        assert_eq!(store.selected_note_ids, vec!["one"]);
        assert_eq!(store.selection_label, "1 selected");
        assert!(
            store
                .other_notes
                .iter()
                .find(|row| row.value.id == "one")
                .is_some_and(|row| row.selected && row.selection_active)
        );

        store.toggle_note_selection("two".to_string());
        assert_eq!(store.selected_note_ids, vec!["one", "two"]);
        assert_eq!(store.selection_label, "2 selected");

        store.toggle_note_selection("one".to_string());
        assert_eq!(store.selected_note_ids, vec!["two"]);
        assert_eq!(store.selection_label, "1 selected");

        store.clear_selection();
        assert!(store.selected_note_ids.is_empty());
        assert!(store.selection_label.is_empty());
    }

    #[test]
    fn rebuild_visible_notes_prunes_deleted_selection() {
        let mut store = KeepStore::default();
        push_row(&mut store, note("kept", false, false, &[]));
        push_row(&mut store, note("deleted", false, false, &[]));

        store.toggle_note_selection("kept".to_string());
        store.toggle_note_selection("deleted".to_string());
        store.notes.retain(|n| n.id != "deleted");

        store.rebuild_visible_notes();

        assert_eq!(store.selected_note_ids, vec!["kept"]);
        assert_eq!(store.selection_label, "1 selected");
    }
}
