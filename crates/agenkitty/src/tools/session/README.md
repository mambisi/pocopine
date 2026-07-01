# Agenkitty Session Framework

The session framework is Agenkitty's public contract for durable agent
identity, model-safe introspection, resumable local runs, and provenance. It
sits on top of `pocopine-agenkit::server::AgentSession`: Agenkit owns the
conversation transcript and branchable thread log, while Agenkitty stores
bounded metadata beside that transcript.

This is deliberately a framework layer. Hosted orchestration, cluster runners,
tenant authorization, dashboards, billing, artifact storage, and production
cleanup workers are downstream product concerns and do not live in this public
crate.

## Design Goals

- Keep model-facing session tools small, bounded, and redacted.
- Keep host operations such as list, export, close, fork, and rewind out of the
  default model tool set.
- Store session metadata in inspectable local backends without creating a
  second transcript store.
- Use stable source references so memory, patch, process, artifacts, and MCP
  tools can cite session provenance without coupling to private runner
  internals.
- Make local durability explicit: hosts choose the root path and can swap the
  metadata store.
- Preserve the public/private boundary: this crate defines contracts and local
  hooks; private orchestration can build on top.

## Architecture

There are three layers:

- `agenkitty-core/src/sessions.rs`: portable DTOs shared by host, CLI, and
  wasm-safe consumers.
- `agenkitty/src/tools/session/*`: model-facing tools, runtime context bridge,
  metadata stores, local JSONL backend, and redaction helpers.
- `agenkitty/src/sessions/mod.rs`: host-facing API over a
  `SessionMetadataStore`.

The runtime flow is:

1. A host opens or creates an Agenkit `AgentSession`.
2. `FrameworkRunner` upserts a `SessionIdentity` for that thread.
3. Agenkit emits `AgentEvent` values during the turn.
4. Agenkitty maps those events to `FrameworkEvent`, redacts model/tool payloads,
   then persists normalized `SessionEvent` records through
   `SessionMetadataStore`.
5. If the model calls a `session.*` tool, `SessionRuntime` injects a one-time
   context token through Agenkit's `before_tool_call` hook.
6. The session tool consumes that token and reads or writes metadata for the
   active thread only.

## Public Core Types

Portable types are in `agenkitty-core`.

`SessionIdentity`

Records durable identity and runtime configuration:

- `thread_id`
- `agent_id`
- `model`
- optional `run_id` and `turn_id`
- optional `principal_key`
- enabled `tool_ids`
- `max_steps_per_turn`
- `capture_policy`
- `transcript_store`
- `metadata_store`
- timestamps
- optional `project_id`

`SessionSourceRef`

The shared provenance vocabulary:

- `Thread`
- `SessionThread`
- `Record`
- `RecordRange`
- `Event`
- `EventRange`
- `Step`
- `ToolCall`
- `Tool`
- `Artifact`
- `Checkpoint`
- `Path`

Use `SessionSourceRef` anywhere another Agenkitty subsystem needs to cite where
knowledge, edits, scripts, MCP calls, artifacts, or cleanup obligations came
from.

`SessionEvent`

A bounded, redacted event in the metadata stream. Events normalize Agenkit and
Agenkitty runtime events into stable kinds:

- `started`
- `assistant_text`
- `tool_started`
- `tool_completed`
- `tool_failed`
- `tool_blocked`
- `compacted`
- `stopped`
- `failed`
- metadata creation events such as notes, summaries, and checkpoints

`SessionNote`, `SessionSummary`, and `SessionCheckpoint`

Model-written metadata. These are not transcript records and do not mutate
workspace state. They are structured annotations linked back to source refs.

`SessionArtifactLink`

Links from a session to downstream artifact systems. The session framework
stores the link and provenance, but not the artifact store implementation.

`SessionClosure`

Terminal metadata for a closed session. Close is idempotent so downstream
cleanup can be keyed on "this call created the closure" instead of racing.

`SessionExport`

Host export payload. Exports include identity, events, notes, summaries,
checkpoints, artifact links, closure, `total_events`, and `events_truncated`.

## Model-Facing Tools

The model-facing set is intentionally small.

`session.info`

Returns active session identity and runtime limits:

- thread id
- agent id
- model
- enabled tools
- max steps
- capture policy
- transcript store kind
- metadata store kind

`session.events`

Returns a bounded event window for the active session. It supports:

- `after_seq`
- inclusive `start_seq` and `end_seq`
- event-kind filters
- explicit limit

Output is redacted and byte-bounded.

`session.note`

Writes a session-scoped note. Inputs require:

- title
- body
- reason
- explicit source refs
- optional tags

`session.summary`

Writes or replaces a compact summary over a bounded event or record range.
Summaries cite covered ranges and source refs. The tool stores a summary
provided by the caller; it does not need to call a model internally.

`session.checkpoint`

Writes a named logical checkpoint. It can point at event ranges, record ranges,
summaries, or source refs. It does not restore files, rewind transcripts, or
claim filesystem rollback.

Host-only operations remain host-only:

- `session.list`
- `session.open`
- `session.export`
- `session.fork`
- `session.rewind`
- `session.close`

## Runtime Context Bridge

`SessionRuntime` owns:

- an `Arc<dyn SessionMetadataStore>`
- one-time context-token storage
- a token sequence for uniqueness

The runner injects context only for known `session.*` tool ids. It first
validates the tool arguments are a JSON object or null, then issues the token.
That ordering avoids leaking unused tokens when the model sends malformed args.

Session tools call `take_context(token)`. Tokens are consumed exactly once, so a
tool call cannot replay a previous session context.

`CurrentSessionContext` provides source-ref helpers:

- `thread_ref`
- `event_ref`
- `event_range_ref`
- `record_ref`
- `record_range_ref`
- `tool_ref`
- `tool_call_ref`

## Metadata Store Contract

`SessionMetadataStore` is the public trait for session metadata backends. It
uses the workspace host-store future shape:

```rust
Pin<Box<dyn Future<Output = AgenkitResult<T>> + Send + 'a>>
```

The operations are grouped by responsibility.

Identity:

- `upsert_identity`
- `identity`
- `list_identities`

Events:

- `append_event`
- `list_events`
- `event_count`

Notes:

- `append_note`
- `read_note`
- `list_notes`

Summaries:

- `write_summary`
- `read_summary`
- `list_summaries`

Checkpoints:

- `write_checkpoint`
- `list_checkpoints`

Links:

- `link_artifact`
- `list_artifact_links`

Closure:

- `close_session`
- `closure`

`close_session` returns `SessionMetadataCloseResult`, which includes the
closure and an `already_closed` flag. Hosts should run session-scoped cleanup
only when `already_closed == false`.

## Backends

`InMemorySessionMetadataStore`

Used for tests and ephemeral runs. It is also the reference implementation for
the trait semantics.

`LocalJsonlSessionMetadataStore`

The local durable backend. It stores append-only JSONL records under a
host-provided root. Hosts choose the root; the framework does not pick a global
user path.

Local JSONL safety properties:

- Empty thread ids are rejected.
- Metadata roots and logs must not be symlinks.
- Log paths are kept under the configured root.
- Filenames are `session-<sha256(thread_id)>.jsonl`, using
  `pocopine-crypto::sha256_hex`.
- Hash-based lowercase filenames avoid path traversal and case-insensitive
  filesystem collisions.
- Reads reload the backing log so another store/process can append metadata and
  the current store observes it.
- A torn trailing JSONL line is tolerated during replay.
- A corrupt complete line fails direct reads for that session.
- Bulk listing skips one corrupt log instead of failing the entire host.

JSONL record kinds:

- identity
- event
- note
- summary
- checkpoint
- artifact link
- closure

## Event Capture

`FrameworkRunner` maps Agenkit runtime events into Agenkitty framework events:

- `AgentEvent::Started`
- `AgentEvent::AssistantText`
- `AgentEvent::ToolStarted`
- `AgentEvent::ToolCompleted`
- `AgentEvent::ToolFailed`
- `AgentEvent::ToolBlocked`
- `AgentEvent::Compacted`
- `AgentEvent::Stopped`
- `AgentEvent::Failed`

Tool events carry:

- provider tool-call id as `SessionSourceRef::ToolCall`
- registry id as `SessionSourceRef::Tool`
- redacted args/results in payload

Assistant text in `AgentRunReport.events` is redacted before the report is
returned. Persisted session events are also redacted.

Compaction events include the folded message count. Source refs to concrete
compaction checkpoints are deferred until host compaction-checkpoint metadata is
implemented.

## Redaction

Redaction is key-aware and conservative.

JSON keys redacted include common credential names such as:

- `api_key`
- `apikey`
- `api-key`
- `x-api-key`
- `authorization`
- `auth`
- `token`
- `access_token`
- `refresh_token`
- `id_token`
- `password`
- `passwd`
- `pwd`
- `secret`
- `client_secret`
- `secret_key`
- `private_key`
- `credential`
- `credentials`

Free-form text is redacted for explicit credential assignments, bearer tokens,
and private-key blocks. Benign prose is preserved; for example, "the secret to
performance is caching" is not blanked.

JSON redaction is depth-capped. Overly deep payloads become
`"[redacted: too deep]"` beyond the cap rather than risking stack overflow.

Store-level link and closure writes redact their own text fields before
persistence, so export is not the only safety layer.

## Host API

`agenkitty::sessions::SessionHost` wraps a `SessionMetadataStore` for host and
CLI operations.

`list(filter)`

Lists sessions filtered by optional principal key and project id. It returns
identity, event count, and closed status.

`open(thread_id)`

Looks up metadata for a resumable thread.

`export(thread_id, options)`

Exports metadata. By default:

- principal key is redacted
- events, notes, summaries, checkpoints, closure, and artifact links are
  redacted
- the event window is bounded
- the newest event tail is returned
- `total_events` reports the full event count
- `events_truncated` tells callers whether older events were omitted

`close(thread_id, reason, source_refs)`

Marks a session closed. Close is idempotent and concurrency-safe at the store
contract level: exactly one racing call should observe `already_closed == false`.

`fork_live_session(session, source_identity)`

Calls Agenkit `AgentSession::fork` for a live session and creates metadata for
the child branch if the transcript store supports forks. The helper sets the
child metadata store label from the `SessionHost` store.

`session.rewind`

Deferred. Rewind should create a new branch from a checkpoint/range, not mutate
history in place.

## Public Boundary

This crate should expose framework contracts and local hooks only. Keep these
outside the public repo:

- hosted control plane
- cluster runner
- tenant authorization
- billing
- dashboard views
- production cleanup workers
- private artifact storage
- private sandbox orchestration

Downstream products can compose those systems around `SessionHost`,
`SessionMetadataStore`, and `SessionSourceRef`.

## Downstream Integration

Other Agenkitty tools should integrate through source refs and session identity.

Memory:

- Store memory provenance as `SessionSourceRef`.
- Cite event ranges, record ranges, tool calls, artifacts, and paths.

Patch:

- Link edits to event/checkpoint refs.
- Avoid making patch metadata a second session log.

Process:

- Scope handles by session identity.
- Close or reap handles from host cleanup when a session closes.

Artifacts:

- Use `link_artifact`.
- Include source refs and promotion policy.
- Let the private product decide storage and retention.

## Deferred Work

- Link patch metadata to session checkpoints/events.
- Link process handles to session lifecycle cleanup.
- Link artifacts to session source refs and promotion policy.
- Add model-facing `session.fork` once fork UX is proven.
- Add host `session.rewind` with clear branch semantics.
- Add session-scoped cleanup hook adapters for process and artifact outputs.
- Add session search over transcript/events.
- Add workspace snapshot integration.
- Keep hosted dashboard/control-plane integration and multi-agent graph
  visualization in the private product layer.

## Testing

Focused gate:

```sh
cargo fmt --all -- --check
cargo test -p agenkitty-core -p agenkitty
cargo clippy -p agenkitty-core -p agenkitty --all-targets -- -D warnings
```

Before pushing broad workspace changes, also run the workspace gates from
`AGENTS.md`.

Important regression coverage:

- model-facing tools reject missing or stale context
- malformed session tool args do not leak context tokens
- event windows are bounded and default filters do not silently return one row
- runner reports redact assistant text and tool payloads
- export redacts all sections and reports truncation
- close reports exactly one newly-created closure under racing calls
- local JSONL survives reload
- local JSONL tolerates torn trailing lines
- local JSONL skips one corrupt log during list
- local JSONL reloads external appends
- local JSONL rejects symlink logs
- hashed log filenames avoid case-insensitive collisions
