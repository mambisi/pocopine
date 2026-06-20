//! Trace-mapping layer: core [`TraceEvent`] -> `pocopine_observe::ObservedEvent`
//! -> `emit_tracing` (RFC-093 Phase 2.3, §D12, §D15 DC-1).
//!
//! This is the single place the runtime touches observability. Core stays
//! observe-neutral; every AI event family is mapped here, with explicit
//! per-field [`FieldPrivacy`] so default redaction (`public_only`) strips
//! anything sensitive. Operational ids, kinds, counts, and durations are
//! `Public`; an error *kind* is `Public` but the error *message* is
//! `Sensitive` (it may quote provider internals).

use pocopine_agenkit_core::{StepKind, StepStatus, TraceEvent};
use pocopine_observe::{
    FieldPrivacy, FieldValue, ObserveContext, ObservedEvent, RedactionPolicy, emit_tracing,
};

fn step_kind_str(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Generation => "generation",
        StepKind::Tool => "tool",
        StepKind::Retrieval => "retrieval",
        StepKind::Embedding => "embedding",
        StepKind::Reducer => "reducer",
        StepKind::Agent => "agent",
        StepKind::Parallel => "parallel",
        StepKind::Custom => "custom",
    }
}

fn step_status_str(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Started => "started",
        StepStatus::Completed => "completed",
        StepStatus::Failed => "failed",
        StepStatus::Cancelled => "cancelled",
    }
}

/// Convert an opaque trace field value to an observability field value.
/// Non-scalars are stringified so they remain inspectable but typed.
fn field_value(value: &serde_json::Value) -> FieldValue {
    match value {
        serde_json::Value::Bool(b) => FieldValue::from(*b),
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(FieldValue::from)
            .or_else(|| n.as_i64().map(FieldValue::from))
            .or_else(|| n.as_f64().map(FieldValue::from))
            .unwrap_or_else(|| FieldValue::from(n.to_string())),
        serde_json::Value::String(s) => FieldValue::from(s.clone()),
        other => FieldValue::from(other.to_string()),
    }
}

/// Map a neutral [`TraceEvent`] to a privacy-labeled [`ObservedEvent`] on the
/// `pocopine.trace` target. Pure (no emission) so it is unit-testable.
pub fn to_observed_event(event: &TraceEvent) -> ObservedEvent {
    let mut observed = ObservedEvent::trace(event.name.clone())
        .context(ObserveContext::default().with_trace_id(event.trace_id.as_str()))
        .field("run_id", event.run_id.as_str(), FieldPrivacy::Public)
        .field("kind", step_kind_str(event.kind), FieldPrivacy::Public)
        .field(
            "status",
            step_status_str(event.status),
            FieldPrivacy::Public,
        );

    if let Some(step_id) = &event.step_id {
        observed = observed.field("step_id", step_id.as_str(), FieldPrivacy::Public);
    }
    if let Some(parent) = &event.parent_step_id {
        observed = observed.field("parent_step_id", parent.as_str(), FieldPrivacy::Public);
    }
    if let Some(group) = &event.parallel_group_id {
        observed = observed.field("parallel_group_id", group.as_str(), FieldPrivacy::Public);
    }
    if let Some(model) = &event.model {
        observed = observed.field("model", model.as_str(), FieldPrivacy::Public);
    }
    if let Some(usage) = &event.usage {
        observed = observed
            .field("input_tokens", usage.input_tokens, FieldPrivacy::Public)
            .field("output_tokens", usage.output_tokens, FieldPrivacy::Public)
            .field(
                "cache_read_tokens",
                usage.cache_read_tokens,
                FieldPrivacy::Public,
            )
            .field(
                "cache_creation_tokens",
                usage.cache_creation_tokens,
                FieldPrivacy::Public,
            )
            .field("total_tokens", usage.total(), FieldPrivacy::Public);
    }
    if let Some(cost) = &event.cost {
        observed = observed
            .field("cost_amount", cost.amount, FieldPrivacy::Public)
            .field("cost_currency", cost.currency.clone(), FieldPrivacy::Public);
    }
    if let Some(error) = &event.error {
        // The kind is safe; the message may quote provider internals.
        observed = observed
            .field("error_kind", error.kind(), FieldPrivacy::Public)
            .field("error_message", error.to_string(), FieldPrivacy::Sensitive);
    }
    if let Some(version) = &event.manifest_version {
        observed = observed.field("manifest_version", version.clone(), FieldPrivacy::Public);
    }
    for (key, value) in &event.fields {
        let privacy = if PUBLIC_FIELD_KEYS.contains(&key.as_str()) {
            FieldPrivacy::Public
        } else {
            FieldPrivacy::Sensitive
        };
        observed = observed.field(key.clone(), field_value(value), privacy);
    }

    observed
}

/// Freeform `TraceEvent::fields` keys that are structural (ids, names, counts,
/// flags) and safe under `public_only` redaction. Any key NOT listed here —
/// including freeform messages and anything a future provider adapter stuffs
/// into the open map — defaults to `Sensitive` and is stripped from public
/// trace projections (§D10).
const PUBLIC_FIELD_KEYS: &[&str] = &[
    "accepted",
    "agent_id",
    "branch",
    "branch_count",
    "candidates",
    "flow_id",
    "group",
    "hit_count",
    "name",
    "reducer_kind",
    "resource_id",
    "retriever_id",
    "streaming",
    "success_count",
    "thread_id",
    "tool_id",
];

/// Map, redact, and emit a [`TraceEvent`] through
/// `pocopine_observe::emit_tracing`.
///
/// This is the runtime's only emission entry point. `emit_tracing` itself does
/// not redact, so the §D10 contract — Sensitive fields (e.g. the raw error
/// message) stripped under the default `public_only` policy — is enforced HERE,
/// before anything reaches a tracing subscriber that may export off-host.
pub fn emit_trace_event(event: &TraceEvent) {
    emit_tracing(&to_observed_event(event).redacted(RedactionPolicy::default()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_agenkit_core::{AgenkitError, ParallelGroupId, RunId, StepId, TraceId, Usage};
    use pocopine_observe::RedactionPolicy;

    fn sample_event() -> TraceEvent {
        TraceEvent::new(
            pocopine_agenkit_core::events::AI_STEP_FAILED,
            RunId::new("run-1"),
            TraceId::new("trace-1"),
            StepKind::Agent,
            StepStatus::Failed,
        )
        .with_step(StepId::new("b1"))
        .with_parent(StepId::new("p"))
        .with_parallel_group(ParallelGroupId::new("candidate_answers"))
        .with_usage(Usage::new(10, 5))
        .with_error(AgenkitError::provider("upstream 503 from secret-host"))
    }

    #[test]
    fn maps_structural_fields_as_public() {
        let observed = to_observed_event(&sample_event());
        assert_eq!(observed.context.trace_id.as_deref(), Some("trace-1"));
        assert!(observed.fields.contains_key("step_id"));
        assert!(observed.fields.contains_key("parent_step_id"));
        assert!(observed.fields.contains_key("parallel_group_id"));
        assert!(observed.fields.contains_key("total_tokens"));
        assert!(observed.fields.contains_key("error_kind"));
    }

    #[test]
    fn error_message_is_sensitive_and_stripped_by_default() {
        let observed = to_observed_event(&sample_event());
        // The raw message is present but labeled Sensitive...
        assert!(observed.fields.contains_key("error_message"));
        // ...and stripped by the default public-only redaction, while the
        // safe error_kind and structural fields survive.
        let redacted = observed.redacted(RedactionPolicy::public_only());
        assert!(!redacted.fields.contains_key("error_message"));
        assert!(redacted.fields.contains_key("error_kind"));
        assert!(redacted.fields.contains_key("parallel_group_id"));
    }

    #[test]
    fn unlisted_freeform_fields_are_sensitive_by_default() {
        // A key outside the structural allowlist (e.g. a freeform message or a
        // future provider-adapter field) must not survive public_only.
        let event = sample_event()
            .with_field("message", "advisory text")
            .with_field("x_provider_debug", "Bearer sk-PLANTED-not-for-clients")
            .with_field("flow_id", "summarize");
        let redacted = to_observed_event(&event).redacted(RedactionPolicy::public_only());
        assert!(!redacted.fields.contains_key("message"));
        assert!(!redacted.fields.contains_key("x_provider_debug"));
        assert!(redacted.fields.contains_key("flow_id"));
    }

    #[test]
    fn emit_does_not_panic_without_subscriber() {
        emit_trace_event(&sample_event());
    }
}
