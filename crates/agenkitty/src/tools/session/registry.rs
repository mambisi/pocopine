use std::sync::Arc;

use pocopine_agenkit::server::AgenkitBuilder;

use super::checkpoint::{SESSION_CHECKPOINT_TOOL_ID, SessionCheckpointTool};
use super::common::SessionRuntime;
use super::events::{SESSION_EVENTS_TOOL_ID, SessionEventsTool};
use super::info::{SESSION_INFO_TOOL_ID, SessionInfoTool};
use super::note::{SESSION_NOTE_TOOL_ID, SessionNoteTool};
use super::search::{SESSION_SEARCH_TOOL_ID, SessionSearchTool};
use super::summary::{SESSION_SUMMARY_TOOL_ID, SessionSummaryTool};

pub fn register_session_tools(
    builder: AgenkitBuilder,
    runtime: Arc<SessionRuntime>,
) -> AgenkitBuilder {
    builder
        .tool(SessionInfoTool::new(runtime.clone()))
        .tool(SessionEventsTool::new(runtime.clone()))
        .tool(SessionSearchTool::new(runtime.clone()))
        .tool(SessionNoteTool::new(runtime.clone()))
        .tool(SessionSummaryTool::new(runtime.clone()))
        .tool(SessionCheckpointTool::new(runtime))
}

pub fn default_session_tool_ids() -> Vec<String> {
    vec![
        SESSION_INFO_TOOL_ID.to_string(),
        SESSION_EVENTS_TOOL_ID.to_string(),
        SESSION_SEARCH_TOOL_ID.to_string(),
    ]
}

pub fn resolve_session_tool_ids(raw: &[String]) -> Result<Vec<String>, String> {
    if raw.is_empty() {
        return Ok(default_session_tool_ids());
    }
    let mut ids = Vec::new();
    for value in raw {
        for id in value.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if !known_session_tool_ids().contains(&id) {
                return Err(format!("unknown session tool `{id}`"));
            }
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids)
}

pub fn known_session_tool_ids() -> [&'static str; 6] {
    [
        SESSION_INFO_TOOL_ID,
        SESSION_EVENTS_TOOL_ID,
        SESSION_SEARCH_TOOL_ID,
        SESSION_NOTE_TOOL_ID,
        SESSION_SUMMARY_TOOL_ID,
        SESSION_CHECKPOINT_TOOL_ID,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tool_id_resolution_validates() {
        assert_eq!(resolve_session_tool_ids(&[]).unwrap().len(), 3);
        assert_eq!(
            resolve_session_tool_ids(&["session.search".to_string()]).unwrap(),
            vec![SESSION_SEARCH_TOOL_ID.to_string()]
        );
        assert_eq!(
            resolve_session_tool_ids(&["session.note".to_string()]).unwrap(),
            vec![SESSION_NOTE_TOOL_ID.to_string()]
        );
        assert_eq!(
            resolve_session_tool_ids(&["session.summary".to_string()]).unwrap(),
            vec![SESSION_SUMMARY_TOOL_ID.to_string()]
        );
        assert_eq!(
            resolve_session_tool_ids(&["session.checkpoint".to_string()]).unwrap(),
            vec![SESSION_CHECKPOINT_TOOL_ID.to_string()]
        );
        assert!(resolve_session_tool_ids(&["session.nope".to_string()]).is_err());
    }
}
