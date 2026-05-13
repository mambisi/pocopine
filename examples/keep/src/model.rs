use serde::{Deserialize, Serialize};

pub const KEEP_STREAM: &str = "keep_notes_for_user";
pub const KEEP_COLLECTION: &str = "keep_notes";
pub const KEEP_TAGS_STREAM: &str = "keep_tags_for_user";
pub const KEEP_TAGS_COLLECTION: &str = "keep_tags";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeepTodo {
    pub id: String,
    pub text: String,
    pub done: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeepNote {
    pub id: String,
    pub title: String,
    pub body: String,
    pub color: String,
    pub pinned: bool,
    pub archived: bool,
    pub todos: Vec<KeepTodo>,
    pub labels: Vec<String>,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeepTag {
    pub id: String,
    pub name: String,
    pub updated_at_ms: u64,
}
