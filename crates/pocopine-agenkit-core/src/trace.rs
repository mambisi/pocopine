//! Neutral trace payloads (RFC-093 Phase 1.4, §D12, §D15 DC-1/DC-1a).
//!
//! Core defines these provider-neutral structures; the facade maps them onto
//! `pocopine_observe::ObservedEvent` + `emit_tracing`, so this crate never
//! depends on `pocopine-observe`. Because observe has no span hierarchy, the
//! parent/child shape is carried by explicit `step_id` / `parent_step_id` /
//! `parallel_group_id` fields (§D15 DC-1a).

use std::collections::BTreeMap;

use crate::{
    AgenkitError, ModelRef, ParallelGroupId, RunId, StepId, StepKind, StepStatus, TraceId,
};

/// Token usage for a model call or an aggregate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    /// Prompt/input tokens, **excluding** any cached-input tokens (those are
    /// counted separately below). Normalized this way so cost is unambiguous
    /// across providers (OpenAI's `prompt_tokens` includes the cached subset;
    /// the wire parser subtracts it).
    pub input_tokens: u64,
    /// Completion/output tokens.
    pub output_tokens: u64,
    /// Cached input tokens read on a prompt-cache hit (billed at the cheaper
    /// cache-read rate).
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache (billed at the cache-creation
    /// rate; 0 for providers that don't bill cache writes, e.g. OpenAI).
    #[serde(default)]
    pub cache_creation_tokens: u64,
}

impl Usage {
    /// Usage from uncached input/output token counts (cache tokens zero).
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }
    }

    /// Set the cache token counts: `cache_read` = prompt-cache hits,
    /// `cache_creation` = prompt-cache writes.
    pub fn with_cache(mut self, cache_read_tokens: u64, cache_creation_tokens: u64) -> Self {
        self.cache_read_tokens = cache_read_tokens;
        self.cache_creation_tokens = cache_creation_tokens;
        self
    }

    /// Total processed tokens (input + output + cache read + cache creation).
    pub fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
    }

    /// Sum two usage records (all four token classes).
    pub fn merge(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cache_read_tokens: self
                .cache_read_tokens
                .saturating_add(other.cache_read_tokens),
            cache_creation_tokens: self
                .cache_creation_tokens
                .saturating_add(other.cache_creation_tokens),
        }
    }
}

/// An estimated monetary cost for a call or aggregate.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CostEstimate {
    /// ISO 4217 currency code (e.g. `"USD"`).
    pub currency: String,
    /// The estimated amount in `currency`.
    pub amount: f64,
}

impl CostEstimate {
    /// A cost estimate.
    pub fn new(currency: impl Into<String>, amount: f64) -> Self {
        Self {
            currency: currency.into(),
            amount,
        }
    }
}

/// A single neutral trace event. The facade translates it to an
/// `ObservedEvent` with the matching event-name constant and `pocopine.trace`
/// target.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TraceEvent {
    /// Stable event name (see [`crate::events`]).
    pub name: String,
    /// The run this event belongs to.
    pub run_id: RunId,
    /// The correlation id shared by every event in one run tree.
    pub trace_id: TraceId,
    /// The step this event describes, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<StepId>,
    /// The parent step, conveying tree structure (§D15 DC-1a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_step_id: Option<StepId>,
    /// The parallel group this step belongs to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_group_id: Option<ParallelGroupId>,
    /// What kind of work the step performs.
    pub kind: StepKind,
    /// The step's lifecycle status.
    pub status: StepStatus,
    /// The model alias involved, if this is a model event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Redacted, privacy-safe metadata fields (ids, counts, durations).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, serde_json::Value>,
    /// Token usage, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Cost estimate, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostEstimate>,
    /// The error, for failure events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AgenkitError>,
    /// The context-manifest version of the flow, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
}

impl TraceEvent {
    /// A new event with the required correlation fields.
    pub fn new(
        name: impl Into<String>,
        run_id: RunId,
        trace_id: TraceId,
        kind: StepKind,
        status: StepStatus,
    ) -> Self {
        Self {
            name: name.into(),
            run_id,
            trace_id,
            step_id: None,
            parent_step_id: None,
            parallel_group_id: None,
            kind,
            status,
            model: None,
            fields: BTreeMap::new(),
            usage: None,
            cost: None,
            error: None,
            manifest_version: None,
        }
    }

    /// Set the step id.
    pub fn with_step(mut self, step_id: StepId) -> Self {
        self.step_id = Some(step_id);
        self
    }

    /// Set the parent step id.
    pub fn with_parent(mut self, parent_step_id: StepId) -> Self {
        self.parent_step_id = Some(parent_step_id);
        self
    }

    /// Set the parallel group id.
    pub fn with_parallel_group(mut self, group: ParallelGroupId) -> Self {
        self.parallel_group_id = Some(group);
        self
    }

    /// Set the model alias.
    pub fn with_model(mut self, model: ModelRef) -> Self {
        self.model = Some(model);
        self
    }

    /// Attach a redacted metadata field.
    pub fn with_field(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }

    /// Attach token usage.
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Attach a cost estimate.
    pub fn with_cost(mut self, cost: CostEstimate) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Attach an error.
    pub fn with_error(mut self, error: AgenkitError) -> Self {
        self.error = Some(error);
        self
    }

    /// Attach the context-manifest version.
    pub fn with_manifest_version(mut self, version: impl Into<String>) -> Self {
        self.manifest_version = Some(version.into());
        self
    }
}

/// A structural node in a trace tree, used for replay and assembling a flow's
/// step hierarchy from emitted events.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TraceSpan {
    /// This step's id.
    pub step_id: StepId,
    /// The parent step, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_step_id: Option<StepId>,
    /// The parallel group this step belongs to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_group_id: Option<ParallelGroupId>,
    /// What kind of work the step performs.
    pub kind: StepKind,
    /// A human-readable step name.
    pub name: String,
    /// The step's final status.
    pub status: StepStatus,
    /// Wall-clock duration, if measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Nested child spans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TraceSpan>,
}

impl TraceSpan {
    /// A new leaf span.
    pub fn new(
        step_id: StepId,
        kind: StepKind,
        name: impl Into<String>,
        status: StepStatus,
    ) -> Self {
        Self {
            step_id,
            parent_step_id: None,
            parallel_group_id: None,
            kind,
            name: name.into(),
            status,
            duration_ms: None,
            children: Vec::new(),
        }
    }

    /// Set the parent step id.
    pub fn with_parent(mut self, parent: StepId) -> Self {
        self.parent_step_id = Some(parent);
        self
    }

    /// Set the parallel group id.
    pub fn with_parallel_group(mut self, group: ParallelGroupId) -> Self {
        self.parallel_group_id = Some(group);
        self
    }

    /// Append a child span.
    pub fn push_child(mut self, child: TraceSpan) -> Self {
        self.children.push(child);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_totals_and_merges() {
        let a = Usage::new(10, 5);
        let b = Usage::new(3, 7);
        assert_eq!(a.total(), 15);
        assert_eq!(a.merge(b), Usage::new(13, 12));
    }

    #[test]
    fn usage_counts_and_merges_cache_tokens() {
        let u = Usage::new(100, 20).with_cache(80, 10);
        assert_eq!(u.cache_read_tokens, 80);
        assert_eq!(u.cache_creation_tokens, 10);
        assert_eq!(u.total(), 210); // 100 + 20 + 80 + 10
        let merged = u.merge(Usage::new(1, 2).with_cache(3, 4));
        assert_eq!(merged, Usage::new(101, 22).with_cache(83, 14));
        // Old payloads without the cache fields still deserialize (serde default).
        let back: Usage = serde_json::from_str(r#"{"input_tokens":5,"output_tokens":6}"#).unwrap();
        assert_eq!(back, Usage::new(5, 6));
    }

    /// Exit-gate proof: a nested-parallel trace tree round-trips with the
    /// structural fields intact and no secret fields present (§D15 DC-1a).
    #[test]
    fn nested_parallel_trace_tree_round_trips() {
        let group = ParallelGroupId::new("candidate_answers");
        let root = TraceSpan::new(
            StepId::new("flow"),
            StepKind::Custom,
            "answer_with_review",
            StepStatus::Completed,
        )
        .push_child(
            TraceSpan::new(
                StepId::new("p"),
                StepKind::Parallel,
                "candidate_answers",
                StepStatus::Completed,
            )
            .push_child(
                TraceSpan::new(
                    StepId::new("b1"),
                    StepKind::Agent,
                    "fast_researcher",
                    StepStatus::Completed,
                )
                .with_parent(StepId::new("p"))
                .with_parallel_group(group.clone()),
            )
            .push_child(
                TraceSpan::new(
                    StepId::new("b2"),
                    StepKind::Agent,
                    "strict_reviewer",
                    StepStatus::Completed,
                )
                .with_parent(StepId::new("p"))
                .with_parallel_group(group.clone()),
            )
            .push_child(
                TraceSpan::new(
                    StepId::new("b3"),
                    StepKind::Agent,
                    "docs_first",
                    StepStatus::Completed,
                )
                .with_parent(StepId::new("p"))
                .with_parallel_group(group.clone()),
            ),
        )
        .push_child(TraceSpan::new(
            StepId::new("r"),
            StepKind::Reducer,
            "judge_and_merge",
            StepStatus::Completed,
        ));

        let json = serde_json::to_string(&root).unwrap();
        let back: TraceSpan = serde_json::from_str(&json).unwrap();
        assert_eq!(root, back);

        // The three parallel branches share one group id and point at parent `p`.
        let parallel = &back.children[0];
        assert_eq!(parallel.children.len(), 3);
        for branch in &parallel.children {
            assert_eq!(branch.parent_step_id.as_ref().unwrap().as_str(), "p");
            assert_eq!(branch.parallel_group_id.as_ref().unwrap(), &group);
        }

        let lower = json.to_lowercase();
        for needle in ["api_key", "authorization", "secret", "bearer", "password"] {
            assert!(!lower.contains(needle), "trace tree leaked {needle:?}");
        }
    }
}
