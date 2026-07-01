# Agenkitty Memory

Memory gives an agent durable, inspectable context across turns, runs, and
worktrees — without tying the public framework to a private hosted memory
service. It is **context the harness can retrieve, write, revise, and forget**;
it is *not* a policy-enforcement boundary and *not* a second transcript store.

Hard guarantees still belong to the tool-policy layer, hooks, sandboxing, and
host configuration. Hosted user/team memory, vector search, and review UIs are
downstream product concerns and do not live in this public crate.

## Relation to the session framework

Agenkit owns the conversation **transcript** (`SessionStore`, a branchable
single-writer thread log) and the active **thread history/checkpoints**
(`AgentThreadStore`). The Agenkitty `session` tool stores bounded session
**metadata** (notes, summaries, checkpoints) beside that transcript.

Memory is different again: it stores **semantic knowledge** derived from
sessions, tools, user input, and artifacts — facts, decisions, procedures,
debugging lessons, preferences. Memory entries *cite* session provenance through
`agenkitty_core::SessionSourceRef` (reusing `session::SessionSourceRefSpec` in
tool schemas) rather than copying transcript payloads. Memory never becomes a
backing store for `AgentThreadStore`.

## Model-facing tools

All five are typed `AiTool`s. They are **opt-in**: registered on demand and
absent from the default tool set. Enable with `--tools memory.search,…`.

| Tool | Side effects | Purpose |
|------|--------------|---------|
| `memory.search` | read-only | Lexical search over the caller's memory; returns bounded snippet hits ordered by relevance, never full bodies. |
| `memory.read` | read-only | Read one entry by id (optionally a historical version) as a bounded view. |
| `memory.write` | side-effecting | Create an entry in the caller's scope. Requires a `reason`. |
| `memory.update` | side-effecting | Revise an entry with optimistic concurrency (`expected_version`) and a `reason`. |
| `memory.forget` | side-effecting | Tombstone an entry. The body becomes unreadable; an audit record remains. |

## Scopes and namespace isolation

| Scope | Namespace | Default availability |
|-------|-----------|----------------------|
| `session` | thread id (or agent id) | available |
| `project` | project id | available |
| `agent` | `<project>::<agent>` | available |
| `user` | host tenant | host-configured only |
| `team` | host org | host-configured only |

The namespace is **derived from the caller**, never supplied by the model. On a
write, `memory.write` computes the namespace from the chosen scope and the run's
`CurrentMemoryContext` (project/agent/thread). Reads, searches, updates, and
forgets only touch namespaces the caller owns; a foreign owner's entry returns
`not_found` (no existence oracle — you cannot probe another namespace by guessing
ids). `user`/`team` writes fail closed unless a host configures a store.

## Entry model

Every entry has an opaque id (`mem-{seq}`), a monotonic `version`, a `scope` +
`namespace`, a `kind` (`fact`, `decision`, `procedure`, `debugging`, `failure`,
`preference`, `instruction`, `artifact_ref`, `trace_summary`), a bounded title
and body, normalized tags, a `source`, optional `source_refs`, an optional
`confidence`, a `retention` policy, host-clock timestamps, and a `reason`.

Title, body, reason, and tags are bounded and normalized at construction. The
constructor and `memory.update` reject obvious **secret-like content** (API-key
labels, bearer tokens, private-key blocks, `.env`-style assignments) and fail
closed — secrets must not be written to memory. Secret *handles* belong to the
secrets tool.

## Storage

A `MemoryStore` trait backs the tools. Two backends ship:

- `InMemoryMemoryStore` — process-local, for tests and session-only harnesses.
- `LocalJsonlMemoryStore` — durable, append-only. All entries live in one
  `memory.jsonl` log under a host-provided root (the `FrameworkRunner` uses
  `<session_root>/memory`). The framework never picks a global user-memory path.

The store is **append-only and auditable**: updates create new revisions and
forgets leave tombstones. The CRUD logic lives once, on a shared `MemoryState`,
as non-committing `plan_*` methods (compute a result plus the records to persist)
paired with `apply_record` (commit one record). The durable backend persists
**before** it commits, so a failed write never leaves a change that vanishes on
reload. Replay tolerates a torn final line (a crash mid-write) but rejects
mid-file corruption, and refuses a symlinked log. SQLite/FTS and vector backends
are deferred adapters.

> v1 assumes a single in-process writer for the durable backend. Multi-process
> coordination and on-disk log compaction are deferred follow-ups.

## Observability

Each mutating verb emits one `tracing` event (`target: "pocopine.log"`, per
RFC-069) carrying **id / scope / kind / version / outcome only** — never the
title, body, tags, or reason.

## Boundaries and deferred work

- **Policy prompting** (Allow/Ask/Deny) is the host tool-permission layer's job
  (`ToolDecision`), not a memory-internal enum. Memory enforces the hard parts —
  scope availability and namespace isolation — directly.
- **`MemoryRetriever`** — an `AiRetriever` over the same store for deterministic
  flow-side retrieval — ships, bound to one caller context and reading only that
  caller's namespaces. It is *not* auto-registered (the model-facing path is
  `memory.search`); a host opts in via `.retriever(…)` or
  `.tool_dyn(retriever.into_tool())`.
- **Lifecycle host APIs** ship as `crate::memory::MemoryHost` (mirroring
  `crate::sessions::SessionHost`): `write_trace_summary` (compaction notes citing
  session records), `promote` (copy to a wider scope, recording a `derived_from`
  relation), and `bootstrap_index` (a byte-budgeted always-on index). Entries
  carry optional relation edges (`derived_from`/`supersedes`/`contradicts`/…).
  Contradiction-repair policy and Agenkit checkpoint cross-referencing remain
  deferred.
- **Shared secret classification** across fs, patch, process, artifacts,
  session, and memory is deferred; v1 memory uses its own conservative
  predicate.
- **Alternate ids and backends** are deferred: content-hash/ULID ids, SQLite
  FTS, vector search, and graph storage can be added as store adapters without
  changing the model-facing tool contract.
- **Import and review flows** are deferred: human memory review UI, cross-agent
  team memory, and importers for `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, and MCP
  memory JSONL belong to host/product integrations.
- Hosted **user/team** memory belongs to the private orchestration product, not
  this crate.
