//! Reducer decisions (RFC-093 Phase 1.4, §D7).
//!
//! A reducer combines parallel branch outputs and records *why* a candidate
//! was accepted, rejected, or merged. For evidence-driven products (e.g. the
//! debugger) the reason should reference concrete evidence, not model claims.

use crate::ReducerKind;

/// The outcome of a reducer/fan-in step.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReducerDecision {
    /// How the decision was made.
    pub kind: ReducerKind,
    /// Whether the reducer accepted a final answer.
    pub accepted: bool,
    /// Why the answer was accepted, rejected, or merged.
    pub reason: String,
    /// The index of the chosen candidate branch, when a single one was picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_index: Option<u32>,
    /// How many candidate branches the reducer considered.
    #[serde(default)]
    pub candidates: u32,
}

impl ReducerDecision {
    /// An accepting decision that chose a candidate by index.
    pub fn accept(kind: ReducerKind, reason: impl Into<String>, chosen_index: u32) -> Self {
        Self {
            kind,
            accepted: true,
            reason: reason.into(),
            chosen_index: Some(chosen_index),
            candidates: 0,
        }
    }

    /// A rejecting decision (no candidate was acceptable).
    pub fn reject(kind: ReducerKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            accepted: false,
            reason: reason.into(),
            chosen_index: None,
            candidates: 0,
        }
    }

    /// Record how many candidates were considered.
    pub fn with_candidates(mut self, candidates: u32) -> Self {
        self.candidates = candidates;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_round_trips() {
        let decision =
            ReducerDecision::accept(ReducerKind::ModelJudge, "claim supported by trace", 1)
                .with_candidates(3);
        assert!(decision.accepted);
        assert_eq!(decision.chosen_index, Some(1));
        let back: ReducerDecision =
            serde_json::from_str(&serde_json::to_string(&decision).unwrap()).unwrap();
        assert_eq!(decision, back);
    }
}
