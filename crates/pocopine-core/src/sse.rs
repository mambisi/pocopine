//! Wire framing for streaming `#[server]` functions (RFC-107).
//!
//! A streaming server fn ([`StreamServerResult`](crate::StreamServerResult))
//! serves its items as Server-Sent Events: each item is one `data: {json}\n\n`
//! frame carrying a serialized [`ServerResult`](crate::ServerResult), and a
//! final `data: [DONE]\n\n` frame ends the stream. The host encodes with
//! [`encode_item`]/[`done_frame`]; the wasm client reassembles the response
//! byte stream with [`SseDecoder`] and interprets each payload with
//! [`decode_payload`]. One codec, both ends.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::server::{Result as ServerResult, ServerError};

/// The terminal sentinel payload that ends a stream.
pub const DONE: &str = "[DONE]";

/// Wrap a payload string as one SSE `data:` frame.
pub fn frame(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// The terminal `data: [DONE]\n\n` frame.
pub fn done_frame() -> String {
    frame(DONE)
}

/// Serialize one stream item to its `data:` payload (a `ServerResult<T>` json),
/// **without** SSE framing — the host adds framing via its SSE encoder
/// (`axum::response::sse::Event::data`), the client decodes it via
/// [`decode_payload`].
///
/// `ServerResult` serializes as `{"Ok":…}` / `{"Err":…}`. If `T`'s own
/// serialization fails (rare), the item is downgraded to an in-band error
/// payload rather than breaking the stream.
pub fn encode_payload<T: Serialize>(item: &ServerResult<T>) -> String {
    serde_json::to_string(item).unwrap_or_else(|e| {
        // The `Err` variant is independent of `T`, so it always serializes and
        // decodes back into a `ServerResult<T>` on the client.
        serde_json::to_string(&ServerResult::<()>::Err(ServerError::App(format!(
            "stream encode: {e}"
        ))))
        .expect("ServerError serializes")
    })
}

/// Encode one stream item as a full SSE frame (`data: {json}\n\n`).
pub fn encode_item<T: Serialize>(item: &ServerResult<T>) -> String {
    frame(&encode_payload(item))
}

/// The outcome of interpreting one SSE `data:` payload.
#[derive(Debug)]
pub enum Decoded<T> {
    /// A streamed item — an `Ok` value or an in-band, terminal `Err`.
    Item(ServerResult<T>),
    /// The terminal `[DONE]` sentinel: the stream is complete.
    Done,
}

/// Interpret one `data:` payload as a stream item or the terminal sentinel.
///
/// A payload that fails to parse becomes an in-band `Network` error item rather
/// than silently dropping — the caller treats it as terminal.
pub fn decode_payload<T: DeserializeOwned>(payload: &str) -> Decoded<T> {
    let payload = payload.trim();
    if payload == DONE {
        return Decoded::Done;
    }
    match serde_json::from_str::<ServerResult<T>>(payload) {
        Ok(item) => Decoded::Item(item),
        Err(e) => Decoded::Item(Err(ServerError::Network(format!("stream decode: {e}")))),
    }
}

/// Reassembles a raw byte stream into SSE `data:` payloads.
///
/// A mirror of the provider SSE framer (kept here so the wasm client decoder
/// needs no agenkit dependency): buffer bytes, split on `\n` (never splits a
/// UTF-8 sequence), tolerate CRLF, skip comment lines, and join an event's
/// `data:` fields on the blank-line boundary.
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Bytes received but not yet split into complete lines.
    bytes: Vec<u8>,
    /// `data:` values accumulated for the in-progress event.
    data: Vec<String>,
}

impl SseDecoder {
    /// A fresh decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw network chunk; returns the joined `data:` payload of every
    /// event whose terminating blank line arrived in (or before) this chunk.
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

    /// At EOF: flush any final line that arrived without a trailing newline,
    /// then any event still buffered without a terminating blank line.
    pub fn flush(&mut self) -> Option<String> {
        if !self.bytes.is_empty() {
            let line = std::mem::take(&mut self.bytes);
            if let Some(event) = self.feed_line(&line) {
                return Some(event);
            }
        }
        self.take_event()
    }

    fn feed_line(&mut self, line: &[u8]) -> Option<String> {
        let text = std::str::from_utf8(line).ok()?;
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.is_empty() {
            return self.take_event();
        }
        if text.starts_with(':') {
            return None; // comment / keep-alive
        }
        if let Some(value) = text.strip_prefix("data:") {
            self.data
                .push(value.strip_prefix(' ').unwrap_or(value).to_string());
        }
        None
    }

    fn take_event(&mut self) -> Option<String> {
        if self.data.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.data).join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_wraps_payload() {
        assert_eq!(frame("hi"), "data: hi\n\n");
        assert_eq!(done_frame(), "data: [DONE]\n\n");
    }

    #[test]
    fn ok_item_round_trips() {
        let item: ServerResult<u32> = Ok(7);
        let wire = encode_item(&item);
        let mut dec = SseDecoder::new();
        let payloads = dec.push(wire.as_bytes());
        assert_eq!(payloads.len(), 1);
        match decode_payload::<u32>(&payloads[0]) {
            Decoded::Item(Ok(v)) => assert_eq!(v, 7),
            other => panic!("expected Ok(7), got {other:?}"),
        }
    }

    #[test]
    fn err_item_round_trips_in_band() {
        let item: ServerResult<u32> = Err(ServerError::App("boom".into()));
        let wire = encode_item(&item);
        let mut dec = SseDecoder::new();
        let payloads = dec.push(wire.as_bytes());
        match decode_payload::<u32>(&payloads[0]) {
            Decoded::Item(Err(ServerError::App(msg))) => assert_eq!(msg, "boom"),
            other => panic!("expected App err, got {other:?}"),
        }
    }

    #[test]
    fn done_sentinel_decodes_to_done() {
        let wire = done_frame();
        let mut dec = SseDecoder::new();
        let payloads = dec.push(wire.as_bytes());
        assert!(matches!(decode_payload::<u32>(&payloads[0]), Decoded::Done));
    }

    #[test]
    fn decoder_reassembles_across_split_chunks() {
        let wire = encode_item(&Ok::<_, ServerError>(42u32));
        let bytes = wire.as_bytes();
        let mid = bytes.len() / 2;
        let mut dec = SseDecoder::new();
        let mut all = dec.push(&bytes[..mid]);
        all.extend(dec.push(&bytes[mid..]));
        // exactly one event emerges once the blank line arrives
        let payload = all.last().expect("event completed");
        match decode_payload::<u32>(payload) {
            Decoded::Item(Ok(v)) => assert_eq!(v, 42),
            other => panic!("expected Ok(42), got {other:?}"),
        }
    }

    #[test]
    fn malformed_payload_is_a_network_item() {
        match decode_payload::<u32>("{not json") {
            Decoded::Item(Err(ServerError::Network(_))) => {}
            other => panic!("expected Network err, got {other:?}"),
        }
    }
}
