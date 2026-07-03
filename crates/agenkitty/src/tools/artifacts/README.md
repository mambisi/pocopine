# Artifact tools

Artifacts store **durable run outputs** — reports, generated code packages,
logs, command outputs — separated from arbitrary workspace edits (`fs.*` /
`patch.*`) and from semantic memory (`memory.*`). Every artifact has a stable
citable id (`art-…`), name/media-type/size/SHA-256 metadata, provenance
source-refs, and a scope. Contents are stored out-of-band by the backend and
read back through bounded windows.

## Tools

| tool | effect | default policy |
|---|---|---|
| `artifact.write` | store text/base64 content, returns a citable id | session scope **Allow**; project scope **Ask** (in-tool, via the host approver) |
| `artifact.read` | bounded content window (or metadata only) by id | Allow |
| `artifact.list` | list the caller's artifacts, newest first | Allow |
| `artifact.link` | attach an existing workspace file by reference | session **Allow**; project **Ask** (in-tool) |
| `artifact.delete` | tombstone: metadata survives for audit, contents removed | **Ask** (dispatch gate) |

The family is **opt-in** like memory: registered through
`register_artifact_tools(builder, runtime)` (or the aggregate
`register_tools_with_all_runtimes_and_artifacts`), never part of the default
read-only tool set.

## Contract

- **Ids are stable and citable.** `artifact.write`/`link` return `art-{seq}`;
  later messages and tool calls reference artifacts by that id.
- **Scopes.** `session` artifacts belong to the current session (reaped with
  session cleanup — the M3 lifecycle hook); `project` artifacts survive across
  sessions and default to `Ask`. The scope split is enforced *inside* the
  tool: the policy evaluator is keyed by tool id and cannot branch on
  arguments, so `ArtifactRuntime` consults the host
  [`ToolApprover`](crate::policy::ToolApprover) for project-scoped mutations
  and fails closed without one.
- **Namespace isolation.** The namespace derives from the caller's runtime
  context (`thread_id` for session scope, `project_id` for project scope),
  never from the model. A foreign artifact looks exactly like a missing one —
  no existence oracle.
- **Bounded everywhere.** Writes cap at 4 MiB; reads return windows of at most
  64 KiB (paginate with `offset`); binary windows return base64 (through
  `pocopine-codec`). Hashing goes through `pocopine-crypto`.
- **Secret-safe.** Content that looks like credential material is rejected
  (the conservative predicate shared with memory); secrets belong to the
  secrets tool. Traces log ids/sizes/outcomes, never content or names.
- **Links are references.** `artifact.link` records metadata + hash for an
  existing workspace file; reads go through the live file, canonicalized and
  confined to the workspace root (symlink escapes are rejected). A link never
  copies bytes.
- **Deletion is a tombstone.** The metadata row survives for audit; contents
  (and the blob, once unreferenced) are removed. Deleted artifacts vanish
  from listings and refuse reads.

## Backends

- `InMemoryArtifactStore` — dev/test default.
- `LocalArtifactStore` — durable: an append-only `artifacts.jsonl` metadata
  log plus content-addressed blobs under `blobs/<sha256>` (identical contents
  share one blob). Every operation persists its record before committing
  in-process state; the log replays on open, so artifacts survive session
  reload. Single in-process writer (multi-process coordination is a deferred
  follow-up, shared with the memory store). The runner mounts it under the
  session root's `artifacts/` directory.

## Deferred

- **Session-close reaping of session-scoped artifacts** and **session ↔
  artifact links / promotion policy** — the M3 integration items (session
  README "Deferred Work").
- **First-class process-output capture** (`process.run` output → artifact
  without a model round-trip). Today the model calls `artifact.write` with
  the output it received.
- **`net.download`** stores fetched binaries here (network README roadmap).
- **Project-persistent artifact GC / retention policy** beyond tombstones.
