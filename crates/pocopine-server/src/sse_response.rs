//! SSE response helpers for streaming `#[server]` functions (RFC-107).
//!
//! The generated host handler for a `-> StreamServerResult<T>` function turns
//! the body's stream into a `text/event-stream` response: each item is one
//! `data: {json}\n\n` frame (a serialized `ServerResult<T>`), a setup error is
//! one error frame, and the stream ends with `data: [DONE]\n\n`. The wasm
//! `fetch::call_stream` client decodes the same frames.

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use futures::stream;
use pocopine_core::server::{Result as ServerResult, ServerError};
use pocopine_core::sse;
use serde::Serialize;

/// Turn a streaming server-function result into a `text/event-stream` response.
///
/// `Ok(stream)` → one `data:` frame per item then `data: [DONE]`; `Err(e)` →
/// one error frame then `[DONE]`. The wasm `fetch::call_stream` client decodes
/// the same frames.
pub fn sse_stream_response<T>(result: pocopine_core::StreamServerResult<T>) -> Response
where
    T: Serialize + Send + 'static,
{
    match result {
        Err(error) => sse_error_response(error),
        Ok(stream) => {
            let frames = stream
                .map(|item| Ok::<Event, std::convert::Infallible>(item_event(&item)))
                .chain(stream::once(async { Ok(done_event()) }));
            Sse::new(frames)
                .keep_alive(KeepAlive::default())
                .into_response()
        }
    }
}

/// A one-error-frame SSE response — a setup/handshake error before any item.
pub fn sse_error_response(error: ServerError) -> Response {
    let event = item_event(&ServerResult::<()>::Err(error));
    let frames = stream::iter([
        Ok::<Event, std::convert::Infallible>(event),
        Ok(done_event()),
    ]);
    Sse::new(frames)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn item_event<T: Serialize>(item: &ServerResult<T>) -> Event {
    // `sse::encode_payload` is the framing-free `ServerResult<T>` json; axum's
    // `Event::data` re-adds the `data:`/`\n\n` framing the client decodes.
    Event::default().data(sse::encode_payload(item))
}

fn done_event() -> Event {
    Event::default().data(sse::DONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_core::sse::{Decoded, SseDecoder, decode_payload};

    async fn collect_items<T: serde::de::DeserializeOwned>(
        resp: Response,
    ) -> (Vec<ServerResult<T>>, bool) {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read sse body");
        let mut dec = SseDecoder::new();
        let mut payloads = dec.push(&bytes);
        if let Some(p) = dec.flush() {
            payloads.push(p);
        }
        let mut items = Vec::new();
        let mut done = false;
        for p in payloads {
            match decode_payload::<T>(&p) {
                Decoded::Item(item) => items.push(item),
                Decoded::Done => done = true,
            }
        }
        (items, done)
    }

    #[tokio::test]
    async fn streams_items_then_done() {
        let stream = stream::iter((0u32..3).map(Ok::<u32, ServerError>)).boxed();
        let resp = sse_stream_response::<u32>(Ok(stream));
        let (items, done) = collect_items::<u32>(resp).await;
        assert!(done, "stream should terminate with [DONE]");
        let values: Vec<u32> = items.into_iter().map(|i| i.unwrap()).collect();
        assert_eq!(values, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn in_band_error_item_is_terminal() {
        let stream = stream::iter(vec![
            Ok::<u32, ServerError>(7),
            Err(ServerError::App("boom".into())),
        ])
        .boxed();
        let resp = sse_stream_response::<u32>(Ok(stream));
        let (items, done) = collect_items::<u32>(resp).await;
        assert!(done);
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], Ok(7)));
        assert!(matches!(&items[1], Err(ServerError::App(m)) if m == "boom"));
    }

    #[tokio::test]
    async fn setup_error_is_one_frame() {
        let resp = sse_error_response(ServerError::Unauthorized("nope".into()));
        let (items, done) = collect_items::<u32>(resp).await;
        assert!(done);
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Err(ServerError::Unauthorized(m)) if m == "nope"));
    }
}
