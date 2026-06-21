use crate::{KeepNote, KeepTag};

use super::{
    KeepStore,
    labels::{normalize_label, normalize_labels},
    view::{card_row_for, command_note_for},
};

impl KeepStore {
    pub fn note_view_signature(&self) -> (String, String, String, Vec<String>, Vec<KeepNote>) {
        (
            self.section_kind.clone(),
            self.section_label.clone(),
            self.search_query.clone(),
            self.selected_note_ids.clone(),
            self.notes.clone(),
        )
    }

    pub fn rebuild_visible_notes(&mut self) {
        self.pinned_notes.clear();
        self.other_notes.clear();
        self.command_notes.clear();
        let existing_ids: Vec<String> = self.notes.iter().map(|n| n.id.clone()).collect();
        self.selected_note_ids
            .retain(|id| existing_ids.iter().any(|existing| existing == id));
        self.update_selection_label();
        let selection_active = !self.selected_note_ids.is_empty();

        for note in &self.notes {
            self.command_notes.push(command_note_for(note));
            if !self.note_is_visible(note) {
                continue;
            }
            let view = card_row_for(note, &self.selected_note_ids, selection_active);
            if note.pinned {
                self.pinned_notes.push(view);
            } else {
                self.other_notes.push(view);
            }
        }
    }

    fn note_is_visible(&self, note: &KeepNote) -> bool {
        let in_section = match self.section_kind.as_str() {
            "notes" => !note.archived,
            "archive" => note.archived,
            "label" => {
                !note.archived && note.labels.iter().any(|label| label == &self.section_label)
            }
            _ => false,
        };
        if !in_section {
            return false;
        }
        let query = self.search_query.trim();
        if query.is_empty() {
            return true;
        }
        let q = query.to_lowercase();
        note.title.to_lowercase().contains(&q)
            || note.body.to_lowercase().contains(&q)
            || note
                .labels
                .iter()
                .any(|label| label.to_lowercase().contains(&q))
            || note
                .todos
                .iter()
                .any(|todo| todo.text.to_lowercase().contains(&q))
    }

    pub fn remember_labels(&mut self, labels: Vec<String>) {
        self.labels = normalize_labels(labels);
        self.sync_missing_tag_rows();
    }

    pub(super) fn remember_label(&mut self, label: &str) -> Option<String> {
        let label = normalize_label(label)?;
        if !self.labels.iter().any(|existing| existing == &label) {
            self.labels.push(label.clone());
        }
        if !self.tags.iter().any(|t| t.name == label) {
            self.next_local_id = self.next_local_id.saturating_add(1);
            let tag = KeepTag {
                id: label.clone(),
                name: label.clone(),
                updated_at_ms: crate::now_ms(),
            };
            let mutation = crate::KeepTag::create(
                tag.id.clone(),
                crate::KeepTagDraft {
                    name: tag.name.clone(),
                    updated_at_ms: tag.updated_at_ms,
                },
            );
            crate::sync::write_tag(mutation, "label");
        }
        Some(label)
    }

    pub fn tag_label_registry(&self) -> Vec<String> {
        let mut labels = Vec::new();
        labels.extend(self.tags.iter().map(|t| t.name.clone()));
        labels.extend(self.notes.iter().flat_map(|n| n.labels.iter().cloned()));
        normalize_labels(labels)
    }

    fn sync_missing_tag_rows(&mut self) {
        let labels = self.labels.clone();
        for label in labels {
            if self.tags.iter().any(|t| t.name == label) {
                continue;
            }
            self.next_local_id = self.next_local_id.saturating_add(1);
            let tag = KeepTag {
                id: label.clone(),
                name: label,
                updated_at_ms: crate::now_ms(),
            };
            let mutation = crate::KeepTag::create(
                tag.id.clone(),
                crate::KeepTagDraft {
                    name: tag.name.clone(),
                    updated_at_ms: tag.updated_at_ms,
                },
            );
            crate::sync::write_tag(mutation, "backfill");
        }
    }
}
