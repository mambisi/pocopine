//! Server-Sent Events (SSE) line framing, shared by the provider crates.
//!
//! Both providers stream `text/event-stream` over a byte stream and need
//! identical transport framing: buffer raw bytes, split on `\n` (which never
//! splits a multi-byte UTF-8 sequence, so a line cut at a newline is always
//! valid UTF-8), tolerate CRLF, skip comment lines, and join an event's
//! one-or-more `data:` fields — dispatching on the blank-line boundary. Only the
//! *event payload* handling differs (OpenAI chat chunks + a `[DONE]` sentinel vs
//! Anthropic's typed events), so that stays in each provider; the framing lives
//! here so a fix can't drift between the two copies.

/// Accumulates SSE bytes and yields each event's joined `data:` payload.
///
/// Feed network chunks with [`SseFraming::push`] (returns every event whose
/// terminating blank line has arrived) and call [`SseFraming::flush`] at EOF to
/// emit a final event that arrived without a trailing blank line.
#[derive(Debug, Default)]
pub struct SseFraming {
    /// Raw bytes received but not yet split into complete lines.
    bytes: Vec<u8>,
    /// `data:` field values accumulated for the in-progress event.
    data: Vec<String>,
}

impl SseFraming {
    /// A fresh framer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw network chunk. Returns the joined `data:` payload of every
    /// event whose terminating blank line arrived in (or before) this chunk.
    /// Comment (`:`-prefixed) and non-`data:` field lines are ignored.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.bytes.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(newline) = self.bytes.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.bytes.drain(..=newline).collect();
            line.pop(); // drop the trailing `\n`
            if let Some(event) = self.feed_line(&line) {
                events.push(event);
            }
        }
        events
    }

    /// At EOF: feed any final line that arrived without a trailing newline, then
    /// emit any event still buffered with no terminating blank line. A
    /// well-behaved stream ends on a blank line, so this only fires when the body
    /// was closed mid-frame.
    pub fn flush(&mut self) -> Option<String> {
        if !self.bytes.is_empty() {
            let line = std::mem::take(&mut self.bytes);
            if let Some(event) = self.feed_line(&line) {
                return Some(event);
            }
        }
        self.take_event()
    }

    /// Process one line (without its trailing `\n`). Returns a complete event's
    /// joined payload when the line is the blank-line boundary.
    fn feed_line(&mut self, line: &[u8]) -> Option<String> {
        let text = std::str::from_utf8(line).ok()?;
        // Tolerate CRLF: strip a `\r` left by splitting a `\r\n` stream on `\n`.
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.is_empty() {
            // Blank line: the event boundary.
            return self.take_event();
        }
        if text.starts_with(':') {
            return None; // comment / keep-alive
        }
        if let Some(value) = text.strip_prefix("data:") {
            // SSE strips exactly one optional leading space after the colon.
            self.data
                .push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
        // Other SSE fields (event:, id:, retry:) are ignored — the data payload
        // is self-describing for both providers.
        None
    }

    fn take_event(&mut self) -> Option<String> {
        (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_one_event_per_blank_line() {
        let mut framing = SseFraming::new();
        let events = framing.push(b"data: a\n\ndata: b\n\n");
        assert_eq!(events, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn joins_multiple_data_fields_for_one_event() {
        let mut framing = SseFraming::new();
        let events = framing.push(b"data: {\"x\":\ndata: 1}\n\n");
        assert_eq!(events, vec!["{\"x\":\n1}".to_string()]);
    }

    #[test]
    fn tolerates_crlf_and_skips_comments() {
        let mut framing = SseFraming::new();
        let events = framing.push(b": keep-alive\r\ndata: hi\r\n\r\n");
        assert_eq!(events, vec!["hi".to_string()]);
    }

    #[test]
    fn reassembles_bytes_split_across_chunks_including_mid_codepoint() {
        // "café 🚀" carries multi-byte chars; feed 3 bytes at a time so they
        // straddle chunk boundaries (a per-chunk `from_utf8_lossy` would corrupt
        // them — line framing on `\n` does not).
        let body = "data: café 🚀\n\n".as_bytes();
        let mut framing = SseFraming::new();
        let mut events = Vec::new();
        for chunk in body.chunks(3) {
            events.extend(framing.push(chunk));
        }
        assert_eq!(events, vec!["café 🚀".to_string()]);
    }

    #[test]
    fn flush_emits_a_trailing_event_without_a_blank_line() {
        let mut framing = SseFraming::new();
        assert!(framing.push(b"data: last").is_empty());
        assert_eq!(framing.flush(), Some("last".to_string()));
        // Idempotent once drained.
        assert_eq!(framing.flush(), None);
    }
}
