//! Context-window survival (roadmap W3).
//!
//! The signal that lets a long session compact-and-continue instead of
//! hard-erroring when the conversation fills the model's context window. Two
//! complementary paths:
//!
//! * **Reactive** — [`is_context_overflow`] recognizes a provider's "input too
//!   long" rejection so the runtime can reclassify it as
//!   [`AgenkitError::ContextOverflow`](pocopine_agenkit_core::AgenkitError::ContextOverflow)
//!   (callers branch on the kind to compact + retry).
//! * **Proactive** — [`context_headroom`] estimates the input against the
//!   model's [`Model::context_window`](super::catalog::Model) *before* the call,
//!   so a session can compact in advance.
//!
//! Both are advisory: the estimate is a coarse heuristic, and this module never
//! truncates or errors on its own — W7 (sessions) owns the compaction decision.

use pocopine_agenkit_core::{ContentPart, Message};

use super::catalog::Model;

/// Tokens kept free below the context window when predicting overflow — covers
/// estimate error + per-request framing the heuristic doesn't model.
const MARGIN: u64 = 256;

/// True when `message` looks like a provider rejection for an over-length
/// request (case-insensitive substring match). Recognizes OpenAI, Anthropic,
/// and generic shapes; ordinary failures (rate limit, auth, 5xx, bad JSON)
/// return false.
pub fn is_context_overflow(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        // OpenAI
        "maximum context length",
        "context_length_exceeded",
        "reduce the length of the messages",
        // Anthropic
        "prompt is too long",
        "input length and `max_tokens` exceed context limit",
        "exceed context",
        // generic / gateways
        "context window",
        "too many tokens",
        "maximum context",
    ];
    PATTERNS.iter().any(|p| m.contains(p))
}

/// Estimate the input tokens a message slice will consume.
///
/// A coarse heuristic — roughly `chars / 4` plus a small per-message framing
/// overhead. A *headroom probe, not a billing figure*; real token counts come
/// from the provider's reported [`Usage`](pocopine_agenkit_core::Usage).
pub fn estimate_input_tokens(messages: &[Message]) -> u64 {
    /// Per-message framing the chars/4 heuristic doesn't capture (role markers,
    /// delimiters).
    const PER_MESSAGE_OVERHEAD: u64 = 4;
    /// A coarse nominal cost per media attachment. Real cost is
    /// provider/resolution-specific; counting the base64 BYTES would massively
    /// over-count, so a flat estimate is closer than either bytes or zero.
    const PER_MEDIA: u64 = 1024;
    messages
        .iter()
        .map(|m| {
            // Size ALL content parts (text, reasoning, JSON, media) — not just the
            // plain text. A thinking- or media-heavy history would otherwise
            // under-count and never trip proactive compaction.
            let mut tokens = PER_MESSAGE_OVERHEAD;
            for part in &m.content.parts {
                tokens = tokens.saturating_add(match part {
                    ContentPart::Text { text } => text.len() as u64 / 4,
                    ContentPart::Thinking { text, signature } => {
                        (text.len() + signature.as_ref().map_or(0, String::len)) as u64 / 4
                    }
                    ContentPart::Json { value } => {
                        serde_json::to_string(value).map_or(0, |s| s.len()) as u64 / 4
                    }
                    ContentPart::Media(_) => PER_MEDIA,
                });
            }
            // A tool-call turn carries its payload in `tool_calls` (the serialized
            // args), and a tool result links by `tool_call_id` — neither rides in
            // `content.parts`. With full-fidelity transcripts these are now in the
            // history, so count them or a tool-heavy thread under-counts and never
            // trips proactive compaction (overflowing at generate instead).
            for call in &m.tool_calls {
                let args_len = serde_json::to_string(&call.args).map_or(0, |s| s.len());
                tokens = tokens.saturating_add((call.tool_id.len() + args_len) as u64 / 4);
            }
            if let Some(id) = &m.tool_call_id {
                tokens = tokens.saturating_add(id.len() as u64 / 4);
            }
            tokens
        })
        .sum()
}

/// The result of a proactive context-window check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextHeadroom {
    /// Estimated input tokens (heuristic — see [`estimate_input_tokens`]).
    pub estimated_input_tokens: u64,
    /// The model's context window, in tokens.
    pub context_window: u32,
    /// The output-token budget reserved for this call.
    pub max_output: u32,
    /// True when estimated input + reserved output + [`MARGIN`] would exceed the
    /// context window — i.e. compaction is likely needed before this call.
    pub over: bool,
}

/// Predict whether `messages` plus a `max_output` budget will overflow `model`'s
/// context window (with a small [`MARGIN`]).
///
/// Approximate and advisory — the caller (W7) owns the compaction decision; this
/// never truncates or errors.
pub fn context_headroom(model: &Model, messages: &[Message], max_output: u32) -> ContextHeadroom {
    let estimated_input_tokens = estimate_input_tokens(messages);
    let over = estimated_input_tokens
        .saturating_add(u64::from(max_output))
        .saturating_add(MARGIN)
        > u64::from(model.context_window);
    ContextHeadroom {
        estimated_input_tokens,
        context_window: model.context_window,
        max_output,
        over,
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog;
    use super::*;
    use pocopine_agenkit_core::Role;

    fn msg(text: &str) -> Message {
        Message::new(Role::User, text)
    }

    #[test]
    fn recognizes_provider_overflow_shapes() {
        assert!(is_context_overflow(
            "This model's maximum context length is 8192 tokens, however you requested 9000"
        ));
        assert!(is_context_overflow("error: context_length_exceeded"));
        assert!(is_context_overflow(
            "Please reduce the length of the messages."
        ));
        assert!(is_context_overflow(
            "prompt is too long: 250000 tokens > 200000 maximum"
        ));
        assert!(is_context_overflow(
            "input length and `max_tokens` exceed context limit"
        ));
    }

    #[test]
    fn rejects_ordinary_errors() {
        assert!(!is_context_overflow("rate limit exceeded, retry after 1s"));
        assert!(!is_context_overflow("invalid api key"));
        assert!(!is_context_overflow("internal server error (500)"));
        assert!(!is_context_overflow("the model produced invalid json"));
    }

    #[test]
    fn estimate_tracks_text_length() {
        assert!(
            estimate_input_tokens(&[msg(&"x".repeat(4000))]) > estimate_input_tokens(&[msg("hi")])
        );
        // ~ chars/4
        let est = estimate_input_tokens(&[msg(&"a".repeat(4000))]);
        assert!((990..=1010).contains(&est), "got {est}");
    }

    #[test]
    fn estimate_counts_reasoning_not_just_text() {
        use pocopine_agenkit_core::{Content, ContentPart};
        // A message whose content is mostly reasoning ("thinking") must count
        // toward the input — `text()` would have reported ~zero.
        let reasoning = Message::new(
            Role::Assistant,
            Content::from_parts(vec![ContentPart::thinking("z".repeat(4000), None)]),
        );
        let est = estimate_input_tokens(&[reasoning]);
        assert!(
            (990..=1010).contains(&est),
            "reasoning under-counted: {est}"
        );
    }

    #[test]
    fn estimate_counts_tool_call_args_not_just_content() {
        use pocopine_agenkit_core::{Content, ToolCall};
        // A tool-call turn with empty text content but a large args payload must
        // count toward the input — otherwise a tool-heavy history under-counts and
        // never trips proactive compaction.
        let big_args = serde_json::json!({ "blob": "z".repeat(4000) });
        let call = Message::assistant_tool_calls(
            Content::default(),
            vec![ToolCall::new("c1", "echo", big_args)],
        );
        let est = estimate_input_tokens(&[call]);
        assert!(est >= 1000, "tool-call args under-counted: {est}");
    }

    #[test]
    fn headroom_flags_over_and_under() {
        let model = catalog::lookup(&"openai/gpt-4o".into()).unwrap(); // 128_000
        let under = context_headroom(model, &[msg("hello there")], 100);
        assert!(!under.over);
        assert_eq!(under.context_window, model.context_window);

        let huge = msg(&"word ".repeat(200_000)); // ~1M chars -> ~250k tokens
        let over = context_headroom(model, &[huge], 1000);
        assert!(over.over);
    }

    #[test]
    fn margin_pushes_a_window_sized_prompt_over() {
        // An input estimated at ~the whole window, with zero output budget, is
        // still flagged: input + MARGIN > window.
        let model = catalog::lookup(&"openai/gpt-4o".into()).unwrap();
        let chars = (model.context_window as usize) * 4; // ~ window tokens of input
        let hr = context_headroom(model, &[msg(&"a".repeat(chars))], 0);
        assert!(
            hr.over,
            "estimated {} vs window {}",
            hr.estimated_input_tokens, hr.context_window
        );
    }
}
