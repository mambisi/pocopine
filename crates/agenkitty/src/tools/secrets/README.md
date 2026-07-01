# Secrets Tools

This module defines Agenkitty's public secret-handling contract. It lets agents
request and use credentials without receiving raw secret values.

The framework is not a vault. It owns safe metadata, transient grant handles,
local development resolvers, redaction, and tool integration points. Hosted
tenant vaults, UI approval flows, runner provisioning, billing, and private
control-plane policy live outside the open framework and plug in through the
resolver/runtime boundary.

## Contract

- Raw secret values are not returned by `secret.*` tools.
- The model sees safe labels, refs, scopes, and opaque grant handles only.
- Every grant is bound to a principal, target tool, purpose, and optional
  destination.
- Expired, revoked, wrong-owner, wrong-tool, and wrong-destination handles fail
  closed before execution.
- Fork/resume exports only inheritable grant metadata and mints fresh handles;
  it never exports secret values.
- Secret values are resolved late inside the target surface that needs them and
  are dropped after that invocation.
- Session state, summaries, checkpoints, events, MCP/process metadata, and
  client/wasm payloads must store only refs, labels, audit metadata, or grants.

## Tools

- `secret.list` returns metadata-only secret entries.
- `secret.request` issues an opaque handle when policy allows or a tuple is
  preauthorized.
- `secret.use` validates a handle for a target tool/purpose/destination without
  revealing the value.
- `secret.revoke` revokes a handle so queued or later work fails before use.

Secret tools are opt-in. The default tool registry does not expose them unless a
host registers a `SecretRuntime`.

## Runtime And Resolvers

`AgentSecretResolver` is the host-side boundary for listing metadata and
resolving approved uses into `SecretString`. Implementations must return values
only for already-approved use contexts.

Built-in resolvers:

- `InMemorySecretResolver` for tests and local harnesses.
- `EnvSecretResolver` for local development, mapping explicit secret refs to
  named environment variables. It does not copy or expose the whole host
  environment.

`SecretRuntime` owns grant state, request policy, preauthorized tuples, expiry,
revocation, audit events, inheritance metadata, and late value resolution.

## Current Integrations

- Process tools accept secret-backed environment overlays. Secret values do not
  go in argv, trace payloads, or model-visible outputs.
- Network tools accept secret-backed headers and bind use to the destination
  origin.
- Filesystem and session tools deny common credential material and redact
  secret-like values before persistence.

Future MCP, provider, deploy, setup, artifact, and blob-store integrations
should follow the same pattern: accept handles, validate purpose and
destination, resolve inside the target tool, redact outputs, and persist
metadata only.

## Policy Boundary

The open framework intentionally stops at contracts and local adapters. A hosted
or private Agenkitty product layer may back `AgentSecretResolver` with a tenant
vault and approval UI, but that layer is responsible for tenant policy,
provisioning, billing, artifact storage, and control-plane APIs.
