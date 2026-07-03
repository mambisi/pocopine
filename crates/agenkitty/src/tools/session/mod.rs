//! Session metadata and introspection tools.

mod checkpoint;
mod common;
mod events;
mod info;
mod local;
mod note;
mod registry;
mod search;
mod source_ref;
mod summary;

pub use checkpoint::{
    SESSION_CHECKPOINT_TOOL_ID, SessionCheckpointInput, SessionCheckpointKindSpec,
    SessionCheckpointOutput, SessionCheckpointTool,
};
pub use common::{
    CurrentSessionContext, InMemorySessionMetadataStore, SessionEventFilter,
    SessionMetadataCloseResult, SessionMetadataFuture, SessionMetadataStore, SessionRuntime,
    current_time_ms, session_event_from_framework,
};
pub(crate) use common::{
    redact_artifact_link, redact_closure, redact_json_value, redact_text_to_limit,
};
pub use events::{
    SESSION_EVENTS_TOOL_ID, SessionEventKindFilter, SessionEventView, SessionEventsInput,
    SessionEventsOutput, SessionEventsTool,
};
pub use info::{SESSION_INFO_TOOL_ID, SessionInfoInput, SessionInfoOutput, SessionInfoTool};
pub use local::LocalJsonlSessionMetadataStore;
pub use note::{SESSION_NOTE_TOOL_ID, SessionNoteInput, SessionNoteOutput, SessionNoteTool};
pub use registry::{
    default_session_tool_ids, known_session_tool_ids, register_session_tools,
    resolve_session_tool_ids,
};
pub use search::{
    SESSION_SEARCH_TOOL_ID, SessionSearchInput, SessionSearchOutput, SessionSearchTool,
};
pub use source_ref::{SessionEventRangeSpec, SessionSourceRefSpec};
pub use summary::{
    SESSION_SUMMARY_TOOL_ID, SessionSummaryInput, SessionSummaryOutput, SessionSummaryTool,
};
