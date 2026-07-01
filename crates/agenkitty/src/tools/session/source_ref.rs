use agenkitty_core::{SessionEventRange, SessionSourceRef};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSourceRefSpec {
    Thread {
        thread_id: String,
    },
    SessionThread {
        session_thread_id: String,
    },
    Record {
        thread_id: String,
        seq: u64,
    },
    RecordRange {
        thread_id: String,
        start_seq: u64,
        end_seq: u64,
    },
    Event {
        seq: u64,
    },
    EventRange {
        start_seq: u64,
        end_seq: u64,
    },
    Step {
        step_id: String,
    },
    ToolCall {
        call_id: String,
        tool_id: String,
    },
    Tool {
        tool_id: String,
    },
    Artifact {
        artifact_id: String,
    },
    Checkpoint {
        checkpoint_id: String,
    },
    Path {
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionEventRangeSpec {
    pub start_seq: u64,
    pub end_seq: u64,
}

impl From<SessionEventRangeSpec> for SessionEventRange {
    fn from(value: SessionEventRangeSpec) -> Self {
        Self {
            start_seq: value.start_seq,
            end_seq: value.end_seq,
        }
    }
}

impl From<SessionEventRange> for SessionEventRangeSpec {
    fn from(value: SessionEventRange) -> Self {
        Self {
            start_seq: value.start_seq,
            end_seq: value.end_seq,
        }
    }
}

impl From<SessionSourceRefSpec> for SessionSourceRef {
    fn from(value: SessionSourceRefSpec) -> Self {
        match value {
            SessionSourceRefSpec::Thread { thread_id } => Self::Thread { thread_id },
            SessionSourceRefSpec::SessionThread { session_thread_id } => {
                Self::SessionThread { session_thread_id }
            }
            SessionSourceRefSpec::Record { thread_id, seq } => Self::Record { thread_id, seq },
            SessionSourceRefSpec::RecordRange {
                thread_id,
                start_seq,
                end_seq,
            } => Self::RecordRange {
                thread_id,
                start_seq,
                end_seq,
            },
            SessionSourceRefSpec::Event { seq } => Self::Event { seq },
            SessionSourceRefSpec::EventRange { start_seq, end_seq } => {
                Self::EventRange { start_seq, end_seq }
            }
            SessionSourceRefSpec::Step { step_id } => Self::Step { step_id },
            SessionSourceRefSpec::ToolCall { call_id, tool_id } => {
                Self::ToolCall { call_id, tool_id }
            }
            SessionSourceRefSpec::Tool { tool_id } => Self::Tool { tool_id },
            SessionSourceRefSpec::Artifact { artifact_id } => Self::Artifact { artifact_id },
            SessionSourceRefSpec::Checkpoint { checkpoint_id } => {
                Self::Checkpoint { checkpoint_id }
            }
            SessionSourceRefSpec::Path { path } => Self::Path { path },
        }
    }
}

impl From<SessionSourceRef> for SessionSourceRefSpec {
    fn from(value: SessionSourceRef) -> Self {
        match value {
            SessionSourceRef::Thread { thread_id } => Self::Thread { thread_id },
            SessionSourceRef::SessionThread { session_thread_id } => {
                Self::SessionThread { session_thread_id }
            }
            SessionSourceRef::Record { thread_id, seq } => Self::Record { thread_id, seq },
            SessionSourceRef::RecordRange {
                thread_id,
                start_seq,
                end_seq,
            } => Self::RecordRange {
                thread_id,
                start_seq,
                end_seq,
            },
            SessionSourceRef::Event { seq } => Self::Event { seq },
            SessionSourceRef::EventRange { start_seq, end_seq } => {
                Self::EventRange { start_seq, end_seq }
            }
            SessionSourceRef::Step { step_id } => Self::Step { step_id },
            SessionSourceRef::ToolCall { call_id, tool_id } => Self::ToolCall { call_id, tool_id },
            SessionSourceRef::Tool { tool_id } => Self::Tool { tool_id },
            SessionSourceRef::Artifact { artifact_id } => Self::Artifact { artifact_id },
            SessionSourceRef::Checkpoint { checkpoint_id } => Self::Checkpoint { checkpoint_id },
            SessionSourceRef::Path { path } => Self::Path { path },
        }
    }
}

pub(super) fn validate_source_refs(
    tool_id: &str,
    refs: Vec<SessionSourceRefSpec>,
) -> AgenkitResult<Vec<SessionSourceRef>> {
    if refs.is_empty() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} source_refs must contain at least one source reference"
        )));
    }
    let mut validated = Vec::with_capacity(refs.len());
    for source_ref in refs {
        validate_source_ref(tool_id, &source_ref)?;
        validated.push(source_ref.into());
    }
    Ok(validated)
}

pub(super) fn validate_event_range(
    tool_id: &str,
    field: &str,
    range: SessionEventRangeSpec,
) -> AgenkitResult<SessionEventRange> {
    if range.start_seq == 0 || range.end_seq == 0 {
        return Err(AgenkitError::validation(format!(
            "{tool_id} {field} start_seq and end_seq must be greater than 0"
        )));
    }
    if range.start_seq > range.end_seq {
        return Err(AgenkitError::validation(format!(
            "{tool_id} {field} start_seq must be <= end_seq"
        )));
    }
    Ok(range.into())
}

fn validate_source_ref(tool_id: &str, source_ref: &SessionSourceRefSpec) -> AgenkitResult<()> {
    match source_ref {
        SessionSourceRefSpec::Thread { thread_id } => {
            non_empty_ref(tool_id, "thread_id", thread_id)
        }
        SessionSourceRefSpec::SessionThread { session_thread_id } => {
            non_empty_ref(tool_id, "session_thread_id", session_thread_id)
        }
        SessionSourceRefSpec::Record { thread_id, seq } => {
            non_empty_ref(tool_id, "thread_id", thread_id)?;
            non_zero_seq(tool_id, "seq", *seq)
        }
        SessionSourceRefSpec::RecordRange {
            thread_id,
            start_seq,
            end_seq,
        } => {
            non_empty_ref(tool_id, "thread_id", thread_id)?;
            valid_range(tool_id, *start_seq, *end_seq)
        }
        SessionSourceRefSpec::Event { seq } => non_zero_seq(tool_id, "seq", *seq),
        SessionSourceRefSpec::EventRange { start_seq, end_seq } => {
            valid_range(tool_id, *start_seq, *end_seq)
        }
        SessionSourceRefSpec::Step { step_id } => non_empty_ref(tool_id, "step_id", step_id),
        SessionSourceRefSpec::ToolCall {
            call_id,
            tool_id: ref_tool_id,
        } => {
            non_empty_ref(tool_id, "call_id", call_id)?;
            non_empty_ref(tool_id, "tool_id", ref_tool_id)
        }
        SessionSourceRefSpec::Tool {
            tool_id: ref_tool_id,
        } => non_empty_ref(tool_id, "tool_id", ref_tool_id),
        SessionSourceRefSpec::Artifact { artifact_id } => {
            non_empty_ref(tool_id, "artifact_id", artifact_id)
        }
        SessionSourceRefSpec::Checkpoint { checkpoint_id } => {
            non_empty_ref(tool_id, "checkpoint_id", checkpoint_id)
        }
        SessionSourceRefSpec::Path { path } => non_empty_ref(tool_id, "path", path),
    }
}

fn non_empty_ref(tool_id: &str, field: &str, value: &str) -> AgenkitResult<()> {
    if value.trim().is_empty() {
        return Err(AgenkitError::validation(format!(
            "{tool_id} source ref {field} is required"
        )));
    }
    Ok(())
}

fn non_zero_seq(tool_id: &str, field: &str, value: u64) -> AgenkitResult<()> {
    if value == 0 {
        return Err(AgenkitError::validation(format!(
            "{tool_id} source ref {field} must be greater than 0"
        )));
    }
    Ok(())
}

fn valid_range(tool_id: &str, start_seq: u64, end_seq: u64) -> AgenkitResult<()> {
    non_zero_seq(tool_id, "start_seq", start_seq)?;
    non_zero_seq(tool_id, "end_seq", end_seq)?;
    if start_seq > end_seq {
        return Err(AgenkitError::validation(format!(
            "{tool_id} source ref start_seq must be <= end_seq"
        )));
    }
    Ok(())
}
