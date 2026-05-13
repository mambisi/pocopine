use serde::{Deserialize, Serialize};

use crate::{KeepNote, KeepTodo};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeepEditorData {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub color: String,
    pub todos: Vec<KeepTodo>,
    pub labels: Vec<String>,
    pub pinned: bool,
}

impl KeepEditorData {
    pub(crate) fn from_note(note: KeepNote) -> Self {
        Self {
            id: note.id,
            kind: if note.todos.is_empty() {
                "text".into()
            } else {
                "checklist".into()
            },
            title: note.title,
            body: note.body,
            color: note.color,
            todos: note.todos,
            labels: note.labels,
            pinned: note.pinned,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct KeepFormNote {
    pub note_id: String,
    pub title: String,
    pub body: String,
    pub color: String,
    pub todos: Vec<KeepTodo>,
    pub labels: Vec<String>,
    pub pinned: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeepCommandNote {
    pub id: String,
    pub title: String,
    pub body: String,
    pub has_todos: bool,
    pub search_value: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeepNoteCardRow {
    pub key: String,
    pub value: KeepNote,
    pub pending: bool,
    pub conflict: bool,
    pub selected: bool,
    pub selection_active: bool,
}

pub(crate) fn command_note_for(note: &KeepNote) -> KeepCommandNote {
    let search_value = [
        note.title.as_str(),
        note.body.as_str(),
        &note.labels.join(" "),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");

    KeepCommandNote {
        id: note.id.clone(),
        title: note.title.clone(),
        body: note.body.clone(),
        has_todos: !note.todos.is_empty(),
        search_value,
    }
}

pub(crate) fn card_row_for(
    row: &pocopine_sync::SyncRow<KeepNote>,
    selected_note_ids: &[String],
    selection_active: bool,
) -> KeepNoteCardRow {
    KeepNoteCardRow {
        key: row.key.to_string(),
        value: row.value.clone(),
        pending: row.pending,
        conflict: row.conflict,
        selected: selected_note_ids.iter().any(|id| id == &row.value.id),
        selection_active,
    }
}

pub(crate) fn format_todo_line(todo: &KeepTodo) -> String {
    if todo.done {
        format!("[x] {}", todo.text)
    } else {
        todo.text.clone()
    }
}

pub(crate) fn parse_todo_line(line: &str) -> (String, bool) {
    if let Some(rest) = line
        .strip_prefix("[x] ")
        .or_else(|| line.strip_prefix("[X] "))
    {
        (rest.trim().to_string(), true)
    } else if let Some(rest) = line.strip_prefix("[ ] ") {
        (rest.trim().to_string(), false)
    } else {
        (line.to_string(), false)
    }
}
