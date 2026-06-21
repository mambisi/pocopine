use pine_richtext::model::Node as RichTextNode;
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

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

// `Eq` is intentionally dropped — `body_state: Option<RichTextNode>`
// transitively contains `serde_json::Value` (attrs are schema-
// dynamic), and `Value` isn't `Eq` because `Number` carries
// `f64`. `PartialEq` still drives the reactive watchers'
// change detection.
//
// `#[query_resource]` (RFC-090) makes `KeepNote` a sync-query row:
// it emits `KeepNote::query()`, the typed `create/update/delete`
// write builders, and the `keep_notes_for_user` module (NAME,
// field markers, predicate, `resource()`). It must sit BEFORE the
// derives so it strips the per-field `#[query_param]` markers
// before serde sees them. The stream name matches `KEEP_STREAM`
// so live-wakeup topics line up between client and server.
#[query_resource(name = "keep_notes_for_user", schema_version = 1, draft = KeepNoteDraft)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeepNote {
    pub id: String,
    pub title: String,
    /// Plain-text preview of the note body. For text notes this
    /// is derived from `body_state` on save (every text block's
    /// content concatenated with `\n` separators). For checklist
    /// notes it stays empty.
    ///
    /// Rendered straight into card tiles, list-detail previews,
    /// and the command-palette search index. The
    /// `text ↔ checklist` toggle also splits on `\n` to turn
    /// paragraphs into todos.
    pub body: String,
    /// Canonical doc state for text notes — a typed
    /// [`pine_richtext::model::Node`] (the doc itself).
    /// `editor.set::<DocNode>(node)` reloads it directly: no
    /// `Value` intermediate, no `to_value` / `from_value` tree
    /// walks. Round-trips don't lose marks, attribute-bearing
    /// nodes, or the keep-specific `title` node distinction.
    ///
    /// `None` for checklist notes (the source of truth is
    /// `todos`) and for legacy text notes saved before the
    /// switch; those fall back to parsing `body` as markdown
    /// on load and get migrated to a `Some(node)` on the next
    /// save. `Node` doesn't impl `Default` (the `name` field
    /// is required), so `Option` is the right "no doc yet"
    /// shape.
    #[serde(default, deserialize_with = "deserialize_optional_node")]
    pub body_state: Option<RichTextNode>,
    pub color: String,
    #[query_param]
    pub pinned: bool,
    #[query_param]
    pub archived: bool,
    pub todos: Vec<KeepTodo>,
    pub labels: Vec<String>,
    pub updated_at_ms: u64,
}

/// Draft payload for `KeepNote` create/update writes — every
/// editable field except the `id` (which is the resource key).
/// `KeepNote::create(id, draft)` / `update(id, draft, version)`
/// take this; `From<(String, KeepNoteDraft)>` builds the optimistic
/// row the engine overlays before the server confirms.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeepNoteDraft {
    pub title: String,
    pub body: String,
    #[serde(default, deserialize_with = "deserialize_optional_node")]
    pub body_state: Option<RichTextNode>,
    pub color: String,
    pub pinned: bool,
    pub archived: bool,
    pub todos: Vec<KeepTodo>,
    pub labels: Vec<String>,
    pub updated_at_ms: u64,
}

impl From<(String, KeepNoteDraft)> for KeepNote {
    fn from((id, draft): (String, KeepNoteDraft)) -> Self {
        Self {
            id,
            title: draft.title,
            body: draft.body,
            body_state: draft.body_state,
            color: draft.color,
            pinned: draft.pinned,
            archived: draft.archived,
            todos: draft.todos,
            labels: draft.labels,
            updated_at_ms: draft.updated_at_ms,
        }
    }
}

impl KeepNote {
    /// Project this note into the draft carried by create/update
    /// writes (everything but the id).
    pub fn to_draft(&self) -> KeepNoteDraft {
        KeepNoteDraft {
            title: self.title.clone(),
            body: self.body.clone(),
            body_state: self.body_state.clone(),
            color: self.color.clone(),
            pinned: self.pinned,
            archived: self.archived,
            todos: self.todos.clone(),
            labels: self.labels.clone(),
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[query_resource(name = "keep_tags_for_user", schema_version = 1, draft = KeepTagDraft)]
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeepTag {
    pub id: String,
    #[query_param]
    pub name: String,
    pub updated_at_ms: u64,
}

/// Draft payload for `KeepTag` writes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepTagDraft {
    pub name: String,
    pub updated_at_ms: u64,
}

impl From<(String, KeepTagDraft)> for KeepTag {
    fn from((id, draft): (String, KeepTagDraft)) -> Self {
        Self {
            id,
            name: draft.name,
            updated_at_ms: draft.updated_at_ms,
        }
    }
}

/// Tolerant deserializer for `body_state` fields.
///
/// The wire format for `body_state` has gone through three
/// shapes during this branch's life:
///
/// 1. Absent / not yet a field — handled by `#[serde(default)]`
///    above, lands as `None`.
/// 2. JSON-encoded **string** (`"body_state": "{...}"`) from an
///    early commit that stored the doc as a serialized string.
///    Strict `Option<Node>` deserialization rejects this and
///    fails the whole row, which is why list-detail row
///    switches were "losing" notes for users who had touched
///    the app during that commit window.
/// 3. JSON **object** (`"body_state": { "type": "doc", … }`) —
///    the canonical shape this branch settled on.
///
/// This deserializer accepts all three: `null` → `None`, empty
/// string → `None` (no useful state), JSON-encoded string →
/// re-parse and try to decode as a `Node`, JSON object → decode
/// directly. Unrecognized shapes fall back to `None` instead of
/// erroring so a single bad row can't poison the whole sync
/// stream.
pub(crate) fn deserialize_optional_node<'de, D>(
    deserializer: D,
) -> Result<Option<RichTextNode>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) => {
            // Phase-1 wire format: the doc was JSON-encoded
            // *inside* a string. Re-parse and try to decode as
            // a Node so existing on-disk rows migrate
            // transparently on next save.
            match serde_json::from_str::<Value>(&s) {
                Ok(Value::Null) => Ok(None),
                Ok(inner) => serde_json::from_value::<RichTextNode>(inner)
                    .map(Some)
                    .map_err(de::Error::custom),
                Err(err) => Err(de::Error::custom(err)),
            }
        }
        Value::Object(_) => serde_json::from_value::<RichTextNode>(value)
            .map(Some)
            .map_err(de::Error::custom),
        _other => Ok(None),
    }
}
