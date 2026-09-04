---
title: "Pocopine 0.2.0 is out — AI flows, live collaboration, and templates that live in Rust"
description: "The release where the interesting work moved behind the component: typed AI flows with three model providers, a CRDT collaboration stack on a WebSocket gateway, server functions that stream, and one trace that runs from a browser click to the model call. Twenty new crates, two breaking changes, and the things we deliberately left undone."
date: 2026-09-04
---

# Pocopine 0.2.0

0.1 was a rendering story. The reactive core, the compiled row plans, the
mutation channel — everything worth writing about happened between a
struct field and a DOM node, and the [signals-first
rewrite](/blogs/pocopine-0-2-0) is the long version of it.

0.2.0 is where that stops being the interesting part. The framework grew
a server side worth the name: flows that call models, sockets that carry
CRDT updates, server functions that arrive item by item, and a trace that
ties a browser click to the model call it caused. Twenty new crates, 389
commits, and two breaking changes.

The promise from 0.1 is unchanged. A handler is still `self.count += 1`.
Nothing below asks you to hold a `Signal<T>`.

## AI flows are a layer, not an example

The largest thing in this release is `pocopine-agenkit`: a way to write
LLM work as typed Rust that produces a trace, instead of as a pile of
JSON and hope.

A tool is a function. A flow is a function. Both are ordinary async Rust
with real input and output types:

```rust
use pocopine_agenkit::prelude::*;
use pocopine_agenkit::{ai_flow, ai_tool};

/// Count the words in a string.
#[ai_tool]
async fn word_count(input: WordCountInput, _ctx: AiToolContext) -> AgenkitResult<u32> {
    Ok(input.text.split_whitespace().count() as u32)
}

#[ai_flow(tools("word_count"))]
async fn summarize(input: SummarizeInput, ctx: AiFlowContext) -> AgenkitResult<Summary> {
    ctx.ai()
        .system("Summarize the prompt as a title and a word count.")
        .prompt(input.prompt)
        .schema::<Summary>()
        .generate_structured()
        .await
}
```

`Summary` is your struct. `generate_structured` validates against its
schema, so the flow's return type is a promise the runtime keeps rather
than a shape you parse and pray over. A tool's description comes from its
doc comment.

Wiring is a builder plus a server plugin:

```rust
let agenkit = Agenkit::builder()
    // Credentials read from ANTHROPIC_API_KEY — server-only.
    .provider(AnthropicProvider::from_env("anthropic").expect("ANTHROPIC_API_KEY"))
    .default_model(models::anthropic::CLAUDE_OPUS_4_8)
    .tool(WordCount)
    .flow(Summarize)
    .build()
    .unwrap();
```

Then a flow is reachable the same way anything else on the server is —
through a `#[server]` function, with no new transport to learn:

```rust
#[server(public)]
pub async fn summarize(input: SummarizeInput) -> ServerResult<Summary> {
    active_plugin::<Agenkit>().expect("agenkit_server_plugin installed")
        .flow(Summarize).input(input).run().await
        .map_err(|e| to_server_error(&e))
}
```

Three provider crates ship: `pocopine-agenkit-anthropic` (native Messages
API, forced-tool structured output, native SSE), `pocopine-agenkit-oai`
for anything speaking OpenAI's chat-completions shape — OpenRouter,
Together, Groq, Ollama, vLLM — and `pocopine-agenkit-qwen` for
Qwen/DashScope. Model handles are typed and generated from a 151-entry
catalog, so `models::anthropic::CLAUDE_OPUS_4_8` is a name the compiler
checks rather than a string you spell right.

Three things about the design are worth stating plainly, because they are
the parts that took the longest:

**The principal is scoped, not passed.** `agenkit_server_plugin` installs
a layer that scopes the request's `Principal` for the whole request, so
tools, retrieval, and threads run as the caller. Your `#[server]` body
never touches identity, which means it cannot forget to.

**Streaming exposure is a ceiling, not a request.** A flow declares how
much of its interior may cross the wire — `FinalOnly` by default, then
`OutputDeltas`, `Progress`, `DebugSafe`. What a caller actually receives
is the AND of the author's ceiling and the caller's request. Reasoning
text is never something a client can talk its way into.

**There is no `#[ai_agent]`.** Agents are the `AiAgent` trait with an
`impl` block. We tried the macro and it bought nothing a trait didn't.

Agenkit is not re-exported through the `pocopine` umbrella yet — apps
depend on the crates directly while the surface settles. That is a
deliberate signal about stability, not an oversight.

## Server functions stream

A server function that returns a stream is a server function. There is no
attribute to remember:

```rust
#[pocopine::server(public)]
async fn count(n: u32) -> StreamServerResult<u32> {
    Ok(futures::stream::iter((0..n).map(Ok::<u32, ServerError>)).boxed())
}
```

The macro reads the return type and picks the client stub, the same way
it already inferred the error type. Calling it looks like what it is:

```rust
let mut items = count(3).await?;
while let Some(item) = items.next().await { /* item: ServerResult<u32> */ }
```

The error model is the part worth internalising. The outer `Result` is
the handshake and fails only when the call never produced a stream at
all. After that, every item is its own `Result`, and a mid-stream `Err`
is terminal. So a stream that dies halfway through is not an exception
you catch around the loop — it is the last item you receive.

Cancellation is free and needs no protocol: the client disconnects, the
SSE body drops, the producer's sender drops, your future stops.

Agenkit's own streaming route was deleted when this landed. A flow
streams because a server function streams.

## Collaboration, on a gateway that owns nothing

Two crates, deliberately split.

`pocopine-realtime` is a payload-agnostic bidirectional WebSocket
gateway. It owns transport and nothing else: binary frames tagged with an
opaque subprotocol id, heartbeats with zombie detection, per-subscription
sequence numbers with `Resume` replay, per-topic authorization, and
fan-out behind a trait that is either in-process or Redis-backed.

`pocopine-collab` is a consumer of it, built on `yrs`. A document is a
topic, a Yjs update is a message, an editor is a subscriber.

```rust
let gateway = WsGateway::local()
    .allow_all_topics()
    .with_collab(compatibility::identity());

let router = Router::new()
    .merge(routes(gateway))
    .fallback_service(static_files(manifest_dir));
```

The interesting line is `with_collab`. Collaboration needs the document
loop and the socket to share one fan-out; wire them by hand and you get
two, at which point replicas diverge silently and you find out days later
from a user. `with_collab` folds the gateway's own fan-out in, so the
requirement is not documented — it is unstateable.

Browsers cannot set headers on a WebSocket upgrade, so authenticated
sessions take the token explicitly:

```rust
let client = RealtimeClient::connect_with_token("/__pocopine/ws/v1", token)?;
```

Honest limits: the worked example is `examples/collab-canvas`, and it
drives the DOM imperatively rather than through a component. The
component-level path is `pine-richtext-collab`, and it does not yet have
an end-to-end example. If you build on collab today, the canvas is your
reference.

## Templates live in Rust now

A small component no longer needs a second file:

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "obs-frontend-shell",
    template = poco! {
    <div class="shell">
      <header><h1>Frontend observability</h1></header>
      <main><pp-outlet></pp-outlet></main>
    </div>
    }
)]
pub struct ObsFrontendShell {}
```

That is real HTML, not a DSL — same parser as a `.poco` file, same
directives, same `{{ }}`. It survives byte for byte, indentation and
comments included.

The catch is that Rust's lexer runs before any macro does, so a template
whose text contains an apostrophe or an em dash never reaches us. We
measured it against the repo's own 359 templates: 275 lex unchanged. For
the rest, quoting the run of text fixes it, because a string literal is
one opaque token:

```rust
poco! { <p>"Don't stop — © 2026"</p> }
```

The [full story is its own post](/blogs/inline-poco-templates), including
the two times we decided not to be clever about it.

`pocopine fmt` keeps this consistent so it does not become a matter of
taste. Small templates get inlined, large ones get extracted to their own
file, and markup gets reindented — in `.poco` files and `poco!` bodies
alike, with clippy-style `off`/`warn`/`fix` levels per rule:

```toml
[package.metadata.pocopine.fmt]
inline-threshold = 150
inline-small-templates = "fix"
extract-large-inline = "warn"
format-markup = "fix"
print-width = 120
```

`pocopine fmt --check` is the CI shape. One rule we specified and then
dropped after measuring: auto-escaping text so more templates could
inline. Thirty-five eligible templates already contain HTML entities, and
escaping text holding `&amp;` would have rendered a visible `&amp;amp;`.
**v1 never edits template content** — it inlines only what already lexes,
and reports the rest.

## One trace, click to model call

Before this release the workspace emitted roughly 270 tracing events and
exactly one span, which meant an OpenTelemetry exporter had almost
nothing to attach them to. 0.2.0 adds a closed set of span names and a
trunk that everything hangs from:

```
pocopine.client.navigation            one page view in the browser
 └ pocopine.client.server_function
      └ pocopine.http.request         covers the response body, not just headers
           └ pocopine.server_function
                └ pocopine.ai.run → ai.step → ai.model / ai.tool

pocopine.job.run                      linked back to the trace that enqueued it
pocopine.realtime.session → .message  for the life of a socket
```

Events did not change shape; spans were added beside them. Field names
follow OpenTelemetry semantic conventions where one exists, so
`http.route` and `gen_ai.request.model` mean in your backend what they
mean everywhere else.

Two headers make it one trace instead of two stories. Every response
carries `x-request-id`; every server-function call carries a per-page-load
session id. With OTLP enabled, the browser sends a `traceparent` and
ships its own closed spans to a relay endpoint, which re-emits them under
the client's ids — so a trace really does start at the click. The browser
sends no trace context unless that relay is on, because a client span
nobody receives would make every server trace the child of a missing
parent.

## Smaller, still worth knowing

- **Compile-time template paths.** A `{{ user.nmae }}` is now a
  compile error instead of a blank span at runtime.
- **Multi-field watch.** `#[watch(a, b, c)]` instead of three functions
  that call the same one.
- **Scoped stores.** State that belongs to a subtree rather than the app.
- **Asset pipeline.** `assets/` syncs to an S3-compatible bucket under
  content-addressed keys, and markdown/templates rewrite to the CDN URL.
- **Native desktop.** `pocopine native dev` runs the app in a Tauri
  webview with `#[server]` functions served in-process.
- **Cloudflare Pages** joins Railway and Render as a deploy target.
- **`pocopine fmt`, `pocopine assets`, `pocopine native`** are the new
  CLI verbs; `pocopine lsp` now backs the VS Code extension.

## Breaking changes

**`template_inline` is gone**, replaced by `poco!`. The old form took a
string literal and could not be a template — no highlighting, no
formatting, no parser. Mechanical fix: `template_inline = "<p>hi</p>"`
becomes `template = poco! { <p>hi</p> }`, and `pocopine fmt` will do it
for you.

**The legacy sync collection client is gone.** `SyncClient`,
`SyncCollection`, and `CollectionState` are replaced by `QueryClient` /
`QueryView` and the `Source` trait. This is a real migration, not a
rename, and it has [its own
guide](/legacy-sync-to-query-migration); `examples/keep` is the worked
reference.

## What we did not do

Server-side rendering is not in this release. It remains the headline of
the 0.2.x line, and the ladder to it is deliberate: formatter parity
first, then static SSR, then structural hydration.

Agenkit has no runnable example that wires a real provider to a browser
client — the shipped example runs in-process against a mock. Collab has
no component-level example. Both are the next things to write, and both
are the reason those surfaces are not in the umbrella crate yet.

The crates are not on crates.io yet. Install the CLI from the release
binary or from source:

```bash
curl -fsSL https://pocopine.dev/install.sh | sh
pocopine --version   # pocopine 0.2.0
```
