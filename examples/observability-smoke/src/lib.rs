//! Minimal host-side observability example.
//!
//! The server function intentionally logs only derived fields. Request text can
//! contain user data, so the telemetry path should prove shape and timing
//! without copying raw payloads into logs or spans.

use pocopine::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use pocopine_server::axum::{Router, response::Html, routing::get};
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use tracing::Instrument as _;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EchoRequest {
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EchoResponse {
    pub message_len: usize,
    pub accepted: bool,
}

#[pocopine::server(public, path = "/_pocopine/observe_echo")]
pub async fn observe_echo(request: EchoRequest) -> ServerResult<EchoResponse> {
    let message_len = request.message.len();
    let span = tracing::info_span!(
        target: "pocopine.trace",
        "observability_smoke.echo",
        message_len,
    );

    async move {
        tracing::info!(
            target: "pocopine.log",
            message_len,
            "observability smoke request accepted"
        );

        Ok(EchoResponse {
            message_len,
            accepted: true,
        })
    }
    .instrument(span)
    .await
}

#[cfg(not(target_arch = "wasm32"))]
pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
}

#[cfg(not(target_arch = "wasm32"))]
async fn index() -> Html<&'static str> {
    tracing::info!(target: "pocopine.trace", route = "/", "served OTLP smoke page");
    Html(INDEX)
}

#[cfg(not(target_arch = "wasm32"))]
async fn health() -> &'static str {
    tracing::info!(target: "pocopine.trace", route = "/health", "OTLP smoke health check");
    "ok\n"
}

#[cfg(not(target_arch = "wasm32"))]
const INDEX: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>pocopine OTLP smoke</title>
  <style>
    :root {
      color-scheme: light;
      --bg: #f6f7f8;
      --panel: #ffffff;
      --text: #1d252d;
      --muted: #5d6874;
      --accent: #0b6f6a;
      --border: #d8dee4;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      background: var(--bg);
      color: var(--text);
      font: 16px/1.5 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      display: grid;
      place-items: center;
      padding: 2rem;
    }
    main {
      width: min(680px, 100%);
      background: var(--panel);
      border: 1px solid var(--border);
      border-radius: 8px;
      padding: 1.5rem;
      box-shadow: 0 18px 40px rgba(17, 24, 39, 0.08);
    }
    h1 { margin: 0 0 0.4rem; font-size: 1.35rem; }
    p { margin: 0 0 1rem; color: var(--muted); }
    label { display: block; font-weight: 650; margin-bottom: 0.4rem; }
    textarea {
      width: 100%;
      min-height: 7rem;
      resize: vertical;
      border: 1px solid var(--border);
      border-radius: 6px;
      padding: 0.75rem;
      font: inherit;
    }
    button {
      margin-top: 0.8rem;
      border: 0;
      border-radius: 6px;
      background: var(--accent);
      color: white;
      padding: 0.65rem 0.9rem;
      font: inherit;
      font-weight: 700;
      cursor: pointer;
    }
    output {
      display: block;
      min-height: 2.5rem;
      margin-top: 1rem;
      padding: 0.75rem;
      border: 1px solid var(--border);
      border-radius: 6px;
      color: var(--muted);
      white-space: pre-wrap;
    }
  </style>
</head>
<body>
  <main>
    <h1>pocopine OTLP smoke</h1>
    <p>Submits one generated server-function request and emits local logs plus OTLP traces.</p>
    <form id="smoke-form">
      <label for="message">Message</label>
      <textarea id="message">hello observability</textarea>
      <button type="submit">Send request</button>
    </form>
    <output id="result">Waiting for a request.</output>
  </main>
  <script>
    const form = document.querySelector("#smoke-form");
    const message = document.querySelector("#message");
    const result = document.querySelector("#result");

    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      result.textContent = "Sending...";
      const response = await fetch("/_pocopine/observe_echo", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify([{ message: message.value }]),
      });
      const body = await response.json();
      result.textContent = JSON.stringify(body, null, 2);
    });
  </script>
</body>
</html>
"##;
