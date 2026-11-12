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
//! errors on its own — W7 (sessions) owns the compaction decision.
//!
//! Within a turn, where compaction cannot run, the loop calls [`trim_turn`]
//! before each model call: the oldest tool outputs the turn produced give way
//! to a short placeholder until the request fits again.

use pocopine_agenkit_core::{Content, ContentPart, Message, Role, ToolDescriptor};

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

/// Estimate the tokens a request spends outside its messages: the system
/// prompt and the tool definitions.
pub(crate) fn estimate_fixed_tokens(system: Option<&str>, tools: &[ToolDescriptor]) -> u64 {
    let tools: usize = tools
        .iter()
        .map(|t| serde_json::to_string(t).map_or(0, |s| s.len()))
        .sum();
    (system.map_or(0, str::len) + tools) as u64 / 4
}

/// The estimated tokens a turn's messages may use on `model`: its window,
/// less the output reserved for the call, the request's `fixed` tokens
/// ([`estimate_fixed_tokens`]) and the [`MARGIN`].
pub(crate) fn room_for_messages(model: &Model, max_output: u32, fixed: u64) -> u64 {
    u64::from(model.context_window)
        .saturating_sub(u64::from(max_output))
        .saturating_sub(fixed)
        .saturating_sub(MARGIN)
}

/// What a tool result says once its output was trimmed by [`trim_turn`].
pub const TRIMMED_TOOL_OUTPUT: &str = "[This tool output was removed to keep the conversation \
     within the model's context window. Run the tool again if you still need it.]";

/// [`trim_turn`] starts once a turn's messages reach this share (in tenths) of
/// the room they have...
const TRIM_AT_TENTHS: u64 = 9;
/// ...and trims down to this share, so the next steps add their results
/// without trimming again at once: each trim edits the conversation's prefix,
/// which providers' prompt caches are keyed on.
const TRIM_TO_TENTHS: u64 = 6;

/// Keep a running turn's `messages` within `room` estimated tokens (see
/// [`room_for_messages`]) by replacing the output of its tool results with
/// [`TRIMMED_TOOL_OUTPUT`], oldest first. Returns how many it trimmed.
///
/// - Only messages from `turn_start` on change: the history before the turn
///   is stored as it was sent, and the turn is stored as its last request
///   sent it, so a resumed conversation replays exactly what the model saw.
/// - The latest batch of results, which the model has not read yet, is
///   trimmed last, and only when trimming every older one is not enough.
/// - Editing a result invalidates the reasoning blocks produced after it on
///   providers that bind them to the conversation before them (Anthropic's
///   newest models), so those blocks are dropped: a trailing run of them may
///   go, a middle one may not. With `keep_last_thinking` (a turn that asks for
///   a thinking budget, which such models refuse) the last assistant
///   message keeps its reasoning, which models that take a budget require
///   while a tool call is pending and do not bind.
pub(crate) fn trim_turn(
    messages: &mut [Message],
    turn_start: usize,
    room: u64,
    keep_last_thinking: bool,
) -> u64 {
    let mut total = estimate_input_tokens(messages);
    if total <= room / 10 * TRIM_AT_TENTHS {
        return 0;
    }
    let target = room / 10 * TRIM_TO_TENTHS;
    // The latest batch: the last run of tool results (steers may follow it).
    let batch_end = messages
        .iter()
        .rposition(|m| m.role == Role::Tool)
        .map_or(turn_start, |i| i + 1)
        .max(turn_start);
    let batch_start = batch_end
        - messages[turn_start..batch_end]
            .iter()
            .rev()
            .take_while(|m| m.role == Role::Tool)
            .count();
    let mut trimmed = 0;
    let mut first = None;
    for (range, goal) in [
        (turn_start..batch_start, target),
        (batch_start..batch_end, room),
    ] {
        for i in range {
            if total <= goal {
                break;
            }
            let message = &mut messages[i];
            if message.role != Role::Tool || message.content.as_text() == TRIMMED_TOOL_OUTPUT {
                continue;
            }
            let before = estimate_input_tokens(std::slice::from_ref(message));
            message.content = Content::text(TRIMMED_TOOL_OUTPUT);
            let after = estimate_input_tokens(std::slice::from_ref(message));
            total = total.saturating_sub(before).saturating_add(after);
            trimmed += 1;
            first.get_or_insert(i);
        }
    }
    if let Some(first) = first {
        let last_assistant = messages.iter().rposition(|m| m.role == Role::Assistant);
        for (i, message) in messages.iter_mut().enumerate().skip(first + 1) {
            if message.role != Role::Assistant || (keep_last_thinking && Some(i) == last_assistant)
            {
                continue;
            }
            message
                .content
                .parts
                .retain(|part| !matches!(part, ContentPart::Thinking { .. }));
        }
    }
    trimmed
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

    /// A turn: the prompt, then `n` steps of reasoning, a tool call and a
    /// result of `chars` characters.
    fn turn(n: usize, chars: usize) -> Vec<Message> {
        use pocopine_agenkit_core::ToolCall;
        let mut messages = vec![msg("before the turn"), msg("go")];
        for i in 0..n {
            messages.push(Message::assistant_tool_calls(
                Content::from_parts(vec![ContentPart::thinking(
                    format!("step {i}"),
                    Some(format!("sig-{i}")),
                )]),
                vec![ToolCall::new(
                    format!("c{i}"),
                    "fs.read",
                    serde_json::json!({}),
                )],
            ));
            messages.push(Message {
                role: Role::Tool,
                content: Content::text(format!("out-{i} {}", "x".repeat(chars))),
                tool_calls: vec![],
                tool_call_id: Some(format!("c{i}")),
            });
        }
        messages
    }

    fn trimmed_at(messages: &[Message]) -> Vec<usize> {
        (0..messages.len())
            .filter(|&i| messages[i].content.as_text() == TRIMMED_TOOL_OUTPUT)
            .collect()
    }

    fn thinking_at(messages: &[Message]) -> Vec<usize> {
        (0..messages.len())
            .filter(|&i| {
                messages[i]
                    .content
                    .parts
                    .iter()
                    .any(|p| p.as_thinking().is_some())
            })
            .collect()
    }

    #[test]
    fn a_turn_within_its_room_is_left_alone() {
        let mut messages = turn(3, 400);
        let before = messages.clone();
        assert_eq!(trim_turn(&mut messages, 2, 10_000, false), 0);
        assert_eq!(messages, before);
    }

    #[test]
    fn trimming_takes_the_oldest_outputs_down_to_six_tenths() {
        // Four results of ~1,000 tokens each in a room of 4,000.
        let mut messages = turn(4, 4_000);
        let trimmed = trim_turn(&mut messages, 2, 4_000, false);
        assert_eq!(trimmed, 2);
        assert_eq!(trimmed_at(&messages), [3, 5]);
        assert!(estimate_input_tokens(&messages) <= 2_400);
        // Reasoning after the first trimmed result is dropped; the step that
        // came before it keeps its block.
        assert_eq!(thinking_at(&messages), [2]);
        // Tool calls and their linkage stay.
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("c0"));
        assert_eq!(messages[6].tool_calls.len(), 1);
        // Trimming again finds nothing more to do.
        assert_eq!(trim_turn(&mut messages, 2, 4_000, false), 0);
    }

    #[test]
    fn nothing_before_the_turn_is_trimmed() {
        let mut messages = turn(3, 4_000);
        // The whole turn so far is history: only what follows may change.
        let start = messages.len();
        messages.push(msg("next prompt"));
        let before = messages.clone();
        assert_eq!(trim_turn(&mut messages, start, 1_000, false), 0);
        assert_eq!(messages, before);
    }

    #[test]
    fn the_latest_batch_goes_last_even_behind_a_steer() {
        let mut messages = turn(2, 4_000);
        messages.push(msg("a steer after the batch"));
        // Room for the latest result alone: the older one goes, the latest
        // (now followed by the steer) stays.
        let trimmed = trim_turn(&mut messages, 2, 2_000, false);
        assert_eq!((trimmed, trimmed_at(&messages)), (1, vec![3]));
        // With less room than the latest result needs, it goes too.
        let trimmed = trim_turn(&mut messages, 2, 600, false);
        assert_eq!((trimmed, trimmed_at(&messages)), (1, vec![3, 5]));
    }

    #[test]
    fn a_thinking_budget_keeps_the_last_reasoning() {
        let mut messages = turn(4, 4_000);
        trim_turn(&mut messages, 2, 4_000, true);
        // The last assistant message (index 8) keeps its block for the
        // pending tool call; the ones between the edit and it are dropped.
        assert_eq!(thinking_at(&messages), [2, 8]);
    }

    #[test]
    fn room_leaves_out_the_output_the_fixed_part_and_the_margin() {
        let model = Model::new("test/room", 10_000, 2_000);
        let tools = [ToolDescriptor::new("fs.read", "Read a file")];
        let fixed = estimate_fixed_tokens(Some(&"s".repeat(400)), &tools);
        assert!(fixed > 100);
        assert_eq!(
            room_for_messages(&model, 1_000, fixed),
            10_000 - 1_000 - fixed - MARGIN
        );
        assert_eq!(room_for_messages(&model, 20_000, fixed), 0);
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
