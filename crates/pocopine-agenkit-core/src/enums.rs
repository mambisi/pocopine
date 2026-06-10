//! Payload-free leaf enums (RFC-093 Phase 1.1).
//!
//! These carry no other Agenkit types, so everything downstream can embed
//! them freely. All serialize `snake_case` for stable, inspectable payloads.

/// Who authored a [`crate::Message`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System / developer instructions.
    System,
    /// End-user input.
    User,
    /// Model output.
    Assistant,
    /// A tool result fed back to the model.
    Tool,
}

/// The kind of work a flow [`step`](crate::StepId) performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// A model generation call.
    Generation,
    /// A tool invocation.
    Tool,
    /// A retrieval/RAG lookup.
    Retrieval,
    /// An embedding call.
    Embedding,
    /// A reducer / fan-in checker.
    Reducer,
    /// A typed agent run.
    Agent,
    /// A parallel fan-out group.
    Parallel,
    /// Arbitrary traced app work wrapped in `ctx.step(...)`.
    Custom,
}

/// The lifecycle status of a step or branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Started but not finished.
    Started,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Aborted (flow cancelled or a losing parallel branch).
    Cancelled,
}

/// Whether a tool may mutate state. Read-only is the safe default (§D5).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffectPolicy {
    /// The tool only reads; safe to retry and run speculatively.
    #[default]
    ReadOnly,
    /// The tool mutates state; must be marked explicitly.
    SideEffecting,
}

/// The category of a framework-mediated resource a flow declares (§D6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// A registered tool.
    Tool,
    /// A registered retriever.
    Retriever,
    /// A registered embedder.
    Embedder,
    /// A typed agent.
    Agent,
    /// An agent thread.
    Thread,
    /// An app state handle.
    State,
    /// A provider alias.
    Provider,
    /// A model alias.
    Model,
}

/// How a reducer reached its decision (§D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReducerKind {
    /// A model acted as judge.
    ModelJudge,
    /// A deterministic Rust function.
    Deterministic,
    /// A voting / majority strategy.
    Vote,
    /// A schema validator.
    SchemaValidator,
    /// A product-specific check (e.g. a debugger evidence check).
    Custom,
}

/// How much of a flow's progress a client may observe (§D8).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    /// No progress stream; only the final typed result.
    #[default]
    FinalOnly,
    /// User-visible output deltas plus the final result.
    OutputDeltas,
    /// Redacted step/tool/parallel/reducer status plus the final result.
    Progress,
    /// Progress plus safe trace refs for local debugging; never raw secrets
    /// or hidden reasoning.
    DebugSafe,
}

/// Promise-like join policy for a parallel branch group (§D8).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ParallelJoin {
    /// Require every branch to succeed.
    All,
    /// Wait for every branch and return successes plus failures.
    #[default]
    AllSettled,
    /// Return the first success and cancel the rest.
    FirstSuccess,
    /// Return once at least `n` branches succeed, cancelling the rest.
    Quorum(u32),
}

/// What to do when branches fail (§D7).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum BranchFailurePolicy {
    /// Abort the group on the first failure.
    FailFast,
    /// Collect partial results and continue.
    #[default]
    CollectPartial,
    /// Require at least `n` successful branches or fail the group.
    MinSuccess(u32),
}

/// How long agent-thread state is retained (§D10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRetention {
    /// Dropped at the end of the run.
    Ephemeral,
    /// Kept for the session.
    Session,
    /// Persisted durably (future durable store).
    Durable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(
            serde_json::to_string(&StepKind::Retrieval).unwrap(),
            "\"retrieval\""
        );
        assert_eq!(
            serde_json::to_string(&ToolSideEffectPolicy::SideEffecting).unwrap(),
            "\"side_effecting\""
        );
    }

    #[test]
    fn parallel_join_data_variant_round_trips() {
        let join = ParallelJoin::Quorum(2);
        let json = serde_json::to_string(&join).unwrap();
        assert_eq!(json, "{\"quorum\":2}");
        assert_eq!(serde_json::from_str::<ParallelJoin>(&json).unwrap(), join);
    }

    #[test]
    fn safe_defaults() {
        assert_eq!(
            ToolSideEffectPolicy::default(),
            ToolSideEffectPolicy::ReadOnly
        );
        assert_eq!(StreamMode::default(), StreamMode::FinalOnly);
        assert_eq!(ParallelJoin::default(), ParallelJoin::AllSettled);
    }
}
