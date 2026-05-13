//! Body editor shared by the Keep composer and note dialog.
//!
//! The parent form owns the complete edit buffer. This component only
//! edits the body-shaped fields through `pp-model`, keeping the text
//! and checklist render paths in one place without deciding whether the
//! final save creates or updates a note.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

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
    pub body: String,
    #[model]
    pub todos: Vec<KeepTodo>,
    #[model]
    pub todo_text: String,
    pub next_todo_id: u64,
}

#[handlers]
impl KeepNoteBody {
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
    fn push_todo(&mut self, text: String, done: bool) {
        self.next_todo_id = self.next_todo_id.saturating_add(1);
        self.todos.push(KeepTodo {
            id: format!("todo_{}_{}", crate::now_ms(), self.next_todo_id),
            text,
            done,
        });
    }
}
