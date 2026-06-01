# RFC 080 — Heroku-style deploy contract (process graph + services)

| Field | Value |
|---|---|
| **Status** | Accepted (contract + launcher + Railway/Render adapters landed; Fly.io adapter dropped from v1 — not supported; `--container` build container + static/Cloud Run adapters pending) |
| **Author** | pocopine team |
| **Created** | 2026-05-17 |
| **Related** | [RFC 002 — App / Stores / Servers](./rfc-002-app-stores-servers.md), [RFC 037 — JS bridge](./rfc-037-js-bridge.md), [RFC 067 — Background jobs](./rfc-067-redis-background-jobs.md), [RFC 073 — Yrs collaboration](./rfc-073-yrs-collaboration.md) |
| **Supersedes** | [RFC 041 — `pocopine deploy` targets](./rfc-041-deploy-targets.md) |

## 1. Summary

Define a portable deploy contract — a Procfile/Heroku-style spec in
`Pocopine.toml` plus a single OCI container artefact — and ship
per-host adapters that translate that contract into each platform's
native configuration. Switching hosts changes one flag; the user's
code, `Cargo.toml`, and `Pocopine.toml` do not.

```toml
# Pocopine.toml
[deploy]
mode = "fullstack"                                       # or "static"

[deploy.processes]
web    = { bin = "server", port = 8080, healthcheck = "/healthz", scale = { min = 1, max = 5 } }
worker = { bin = "worker",                                        scale = { min = 1, max = 3 } }
# Collab is not a separate process: its WebSocket route is mounted inside `web`.
# Redis is still required because workers + collab use it; see §2.2.

[deploy.services]
postgres = { required = true }
redis    = { required = true }

[deploy.env]
DATABASE_URL = { from = "secret" }
REDIS_URL    = { from = "secret" }
```

```sh
pocopine deploy --target fly       # fly machines + Fly Postgres + Upstash Redis
pocopine deploy --target railway   # Railway services + managed Postgres + managed Redis
pocopine deploy --target render    # Render services + managed Postgres + managed Redis
```

This RFC supersedes RFC 041. RFC 041 framed adapters as "thin
shell-out wrappers" with zero deployment logic of our own; the Shuttle
shutdown (Jan 2026) and the arrival of workers (RFC 067) and collab
(RFC 073) made that framing untenable. We now own a small contract.

## 2. Motivation

### 2.1 What Shuttle's shutdown taught us

RFC 041 made Shuttle the first-class full-stack target on the basis of
"single cargo command, no Docker, Rust-native." When Shuttle.dev wound
down in 2026, every app that depended on that DX (the
`#[shuttle_runtime::main]` entrypoint, `#[shuttle_shared_db::Postgres]`
annotations, the Shuttle build pipeline) had to be rewritten before it
could be deployed elsewhere. RFC 041 anticipated this risk in §4.1 but
relied on "we encapsulate the annotations" as the firewall — which
wasn't enough, because the *deploy artefact* (a Shuttle build) was
still vendor-shaped.

The lesson: **no vendor's SDK or runtime macro may appear in user code
or in our build output.** The artefact has to be portable. The contract
between the app and the host has to be the artefact + a portable spec,
not a per-vendor entrypoint.

### 2.2 What workers and collab broke

RFC 067 (background jobs with Redis) and RFC 073 (Yrs collaboration
over WebSocket + Redis) turned a pocopine deploy into a **two-process
graph** with **two backing services**:

- `web` — axum, serves the client bundle + `#[server]` POST endpoints + the collab WebSocket route. Collab is **not** a separate binary; its handler is mounted inside `web` and shares the axum router. Multiple `web` replicas are stateless because Redis is the coordination layer.
- `worker` — `pocopine-jobs` consumer; reads Redis Streams, runs `#[job]` functions. Non-public.
- **Redis** — shared by `worker` (queues, scheduled, dead, locks) and the in-`web` collab handler (Streams, Pub/Sub, awareness TTL).
- **Postgres** — application data + collab snapshots.

Two processes, two services. RFC 041's "one container, one process"
mental model can't express even this. Fly, Railway, Render, and Cloud
Run *all* natively model "multiple processes + add-ons" — the
contract just has to surface that shape.

### 2.3 Why not Terraform / Pulumi / Nix

Terraform-class tools operate at the infrastructure layer (VPCs, IAM,
RDS, Redis clusters, DNS). That's the wrong layer. Pocopine apps don't
manage VPCs — they need "give me a Redis URL." The right abstraction
is **app deploy**: process types, env, ports, healthchecks, declared
backing services. That's the Heroku / 12-factor shape, and it has
survived 15 years of host churn precisely because it stops short of
infra.

We are not building a Terraform replacement. The user (or the host's
console) provisions the Postgres instance; we read the URL from a
secret and wire it in.

## 3. Goals & non-goals

### Goals

- One portable spec (`Pocopine.toml [deploy]`) that adapters translate to each host's native config.
- One artefact per mode: static dist *or* an OCI image (multi-process via Procfile-style entrypoint).
- First-class support for the pocopine process graph (`web`, `worker`) and the two backing services it actually uses (`postgres`, `redis`). Collab is a route inside `web`, not a separate process.
- Adapters as a Rust trait, not bash scripts. We control the build and config generation; **adapters talk to host APIs directly** — no auxiliary host CLI install required.
- One-tool install: `cargo install pocopine-cli` is the only required local prerequisite for the default path. A Rust toolchain is needed to build locally (rustup); Docker is **optional**, only required when the user opts into `pocopine build --container` (§4.4) or when an adapter needs `docker build`/`docker push` to ship an image to its host's registry. No `flyctl`, `railway`, `render`, `gcloud`, or `wrangler` in the deploy path.
- Detection-driven gating: refuse incompatible targets at the CLI, never silently produce a broken deploy.
- Per-host escape hatches (`[deploy.fly]`, `[deploy.railway]`, …) for capabilities the portable contract can't reach.

### Non-goals

- Infrastructure provisioning. We do not create Postgres instances, VPCs, IAM roles, or DNS.
- Secrets management. Secrets live in the host's own store (`fly secrets set`, `railway variables set`, `render env`). We print the command on first deploy.
- Universal lowest-common-denominator API. Where hosts genuinely differ (regions, sticky sessions, scale-to-zero semantics), the contract surfaces a host-namespaced override rather than papering over it.
- Cloudflare Workers as a `#[server]` target. Requires a different handler shape; separate RFC.
- Self-hosted Kubernetes / bare-metal. Out of scope for v1 as a deploy *target*; the contract is designed to extend there, but no adapter ships. (Building locally on a Docker-only machine is supported — see §4.4 — that's a separate axis.)
- Plugin / extension protocol for third-party adapters. The `DeployAdapter` trait is intentionally Rust-only and internal. New hosts land as PRs against `pocopine-deploy`'s built-in adapter set; sophisticated users targeting unsupported platforms self-manage their deploy with their own tooling. We do not ship a JSON-over-stdio plugin protocol, a template DSL, or a scriptable adapter format — keeping the surface small, type-safe, and one canonical contribution path are all worth more than the pluggability we'd get back.

## 4. The contract

### 4.1 `Pocopine.toml [deploy]`

```toml
[deploy]
mode = "fullstack"        # "fullstack" | "static"
                          #   static: client bundle only; no #[server], #[job], or collab.
                          #   fullstack: builds an OCI image with the declared processes.

[deploy.processes.web]
bin         = "server"
port        = 8080
healthcheck = "/healthz"
scale       = { min = 1, max = 5 }
public      = true        # default true for processes with `port`; false hides from public ingress.

[deploy.processes.worker]
bin   = "worker"
scale = { min = 1, max = 3 }
                          # no `port`: not addressable from outside.

# Collab has no [deploy.processes.collab] block. The WebSocket handler is
# mounted inside the `web` binary at the route configured by RFC 073. To
# scale collab independently of REST traffic, run more `web` replicas.

[deploy.services.postgres]
required = true           # `pocopine deploy` refuses if the host can't supply one
                          # AND no $DATABASE_URL secret is set.

[deploy.services.redis]
required = true           # same rule; required whenever a `worker` or `collab` process is declared.

[deploy.env]
DATABASE_URL = { from = "secret" }
REDIS_URL    = { from = "secret" }
LOG_LEVEL    = "info"     # literal value; baked into the image
APP_NAME     = { from = "env" }      # passed through from `pocopine deploy`'s shell env

# Host-specific overrides — only what the portable contract can't express.
[deploy.fly]
regions = ["fra", "iad"]
volumes = [{ process = "collab", path = "/data", size_gb = 1 }]

[deploy.railway]
plan = "hobby"

[deploy.render]
plan = "starter"
```

**Rules:**

- The set of processes is whatever the user declares. Standard names (`web`, `worker`) carry conventions (the adapter uses `web` as the public HTTP entry — including any collab WebSocket route mounted inside it; `worker` is non-public). Other names are allowed but treated as generic processes. There is intentionally **no** `collab` process — see §2.2.
- If any `[deploy.processes.<name>]` other than `web` is declared, `mode = "fullstack"`.
- `redis` becomes `required = true` automatically when a `worker` process is declared OR `pocopine-collab` is in the dependency graph (since the in-`web` collab handler needs it). The CLI prints the inferred value.
- `postgres` is required if any `#[server]` function uses pocopine's storage layer or if `pocopine-collab` is present (snapshot persistence). The CLI infers this from the build artefact's metadata.
- Static mode rejects the presence of any `[deploy.processes.*]` other than (optionally) `web` with no `bin` (interpreted as "serve static `dist/`"), and rejects `[deploy.services]` entirely. Static mode is also refused if `pocopine-collab` is in the dependency graph.

### 4.2 The artefact

| Mode | Artefact | How processes start |
|---|---|---|
| static | `dist/` directory (wasm + JS + assets) | host serves files; no entrypoint |
| fullstack | one OCI image; multi-stage Rust build → distroless runtime | image entrypoint is `pocopine-launcher <process>`; host runs N copies, each invoked with the process name |

The image is **one image, many entrypoints**. `pocopine-launcher` is a
~30-LOC shim that reads the process name from argv and execs the
matching binary defined in `[deploy.processes.<name>].bin`. Concrete
implementation in §5.3. This is identical to how Heroku/Procfile,
Fly's `[processes]` block, and Render's "services from the same image"
all model it.

### 4.3 The adapter trait (sketch)

```rust
pub trait DeployAdapter {
    fn name(&self) -> &'static str;
    fn tested_against(&self) -> semver::VersionReq;          // host-API schema/version this adapter is known to work with
    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint>;
    fn render_config(&self, spec: &DeploySpec, out: &mut StagedFiles);
    fn build_artefact(&self, spec: &DeploySpec) -> Result<Artefact>;
    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> Result<DeployOutcome>;
    fn post_deploy_hint(&self, spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint>;
}
```

`DeploySpec` is the parsed `[deploy]` block plus computed values
(inferred services, process bin paths from `Cargo.toml`, build-time
feature flags from §9). The adapter sees an already-normalised spec —
no host-specific reasoning is duplicated across adapters.

`render_config` writes into a staging dir (`StagedFiles`) instead of
the filesystem so `pocopine deploy --dry-run` can print everything
that would be written without touching disk, and so tests can assert
on generated contents without running `docker`. The full method
contract and end-to-end pipeline are in §5; Appendix A shows a
working Railway adapter.

### 4.4 The build container

The artefact in §4.2 is produced by a **canonical build container**
that pocopine publishes:

```
ghcr.io/pocopine/build:<rust-toolchain>-<bundler-version>
```

The image contains everything needed to turn a pocopine source tree
into a deployable artefact and nothing the deploy step needs:

- Pinned Rust toolchain (`rustc`, `cargo`, `rust-std-x86_64`, `rust-std-wasm32-unknown-unknown`).
- `wasm-bindgen-cli`, `trunk`, and other client-bundle tooling at exact versions.
- Common system deps (`pkg-config`, `libssl-dev`, `clang`, `lld`, `git`).
- `pocopine-cli` itself, installed at the version that matches the image tag.
- `sccache` for cross-invocation caching when a host cache volume is mounted.
- **No** host CLIs (no `flyctl`, no `railway`, no `gcloud`). Build is host-agnostic; the deploy step is host-specific and happens *outside* the build container.

**How users invoke it.** `pocopine build` uses the host's local Rust
toolchain by default — that's the fast inner loop for development
(incremental compilation, native `target/` cache, no container
overhead). `--container` is the **opt-in** for reproducible builds,
CI, or users on a fresh machine with no rustup:

```sh
# Default — local Rust toolchain. Fast iteration; assumes rustup + cargo on PATH.
pocopine build

# Opt in to the canonical build container.
pocopine build --container               # runtime = docker (default)
pocopine build --container=docker        # explicit
pocopine build --container=podman        # planned (post-v1)

# Internally, --container expands to:
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/pocopine/build:<v> pocopine build --in-container
```

The `--container` flag takes an optional runtime value: `docker`
(default, the only supported runtime in v1) and `podman` (planned,
rootless-friendly, on the roadmap). Other OCI-compatible runtimes
(`buildah`, `nerdctl`) are not promised — keeping the runtime set
small is intentional, per [[feedback-opinionated]].

Artefacts (`target/release/<binaries>`, `dist/`, `pocopine-build-meta.json`)
land on the host filesystem via the bind mount when in `--container`
mode, or directly under `target/` and `dist/` when building locally.
`pocopine deploy --target <host>` then picks them up exactly the same
way regardless of how they were produced — adapters do not know or
care which path produced the artefact.

**Why opt-in, not default.** Local cargo is meaningfully faster than
containerised builds for the inner dev loop (incremental compilation
re-uses `target/` across invocations without bind-mount overhead;
container runtimes add ~hundreds of ms of startup per build). Forcing
the container by default punishes users with working Rust setups for
the benefit of edge cases. The default optimises the path most
contributors hit dozens of times per session.

**Why ship the container at all.** Three concrete benefits when opted
into:

1. **Zero-Rust onboarding.** A fresh checkout deploys to Fly on a
   brand-new laptop with only `docker` installed:
   `pocopine build --container && pocopine deploy --target fly`. No
   rustup, no cargo, no per-machine toolchain pinning.
2. **Reproducible builds.** The image tag pins toolchain + bundler +
   system libs. Two contributors with different host OSes produce
   byte-identical artefacts when they run the same `<v>`. CI uses
   `--container` by default to lock this in.
3. **Decoupled build/deploy.** A CI pipeline can `pocopine build
   --container` once, push the resulting OCI image (or `dist/`) to a
   registry / S3, and then run `pocopine deploy --target <X>
   --skip-build` on multiple environments without rebuilding. The
   build artefact is the handoff boundary.

**Versioning.** Image tag matches `pocopine-cli`'s release version
plus the toolchain it pins, e.g. `0.7.2-rust-1.84`. Bumping Rust or
the bundler is a major-minor on the image; security patches are
patch-level. `pocopine deploy doctor` warns when the running image
is older than what the current `pocopine-cli` recommends.

**What this is NOT.** The build container is a *build* environment,
not a *deploy* target. Compare:

| | Build container (§4.4) | Runtime image (§4.2) |
|---|---|---|
| Purpose | compile + bundle | run the app in production |
| Base | `rust:1.84-slim` + tooling | distroless |
| Size | ~2 GB (toolchain + libs) | ~30–80 MB (binaries + assets) |
| Where it runs | locally / in CI; never deployed | on Fly / Railway / Render / Cloud Run |
| Entrypoint | `pocopine` CLI | `pocopine-launcher <process>` |

The build container produces the runtime image (and the static
`dist/`); the runtime image is what hosts actually run. Two
distinct artefacts; the build container is the factory, the runtime
image is the product.

## 5. How an adapter works end-to-end

This section is the spec implementers work to. Appendix A shows the
complete prototype source for the Railway adapter.

### 5.1 The pipeline

```
Pocopine.toml [deploy]              user-authored
       │
       ▼
parse + normalise                   pocopine-cli reads build-artefact
       │                            metadata (uses_jobs, uses_collab,
       │                            uses_storage, uses_websocket — see §9)
       │                            and infers required services
       ▼
DeploySpec                          normalised, host-agnostic struct
       │
       ▼
adapter.detect_constraints(spec)    Vec<Refuse | Warn | Hint>
       │                            Refuse → bail. Warn/Hint → print.
       ▼
adapter.render_config(spec, out)    writes Dockerfile, fly.toml, …
       │                            pure; idempotent; testable
       ▼
adapter.build_artefact(spec)        OCI image (fullstack) or dist/ (static)
       │
       ▼
adapter.deploy(spec, artefact)      calls host HTTP/GraphQL API; pushes
       │                            image to host registry; streams output
       │                            through tracing::info!(target="pocopine.log")
       ▼
adapter.post_deploy_hint(...)       prints one-time setup commands the
                                    user still needs to run (provisioning,
                                    secrets) — adapter never runs them
```

`pocopine-cli` owns parsing, normalisation, ordering, and dry-run
short-circuiting. The adapter owns translation.

### 5.2 The trait, in full

The §4.3 sketch is the minimum a working adapter has to implement.
Expanding it with the types involved:

```rust
pub trait DeployAdapter {
    fn name(&self) -> &'static str;
    fn mode(&self) -> AdapterMode;          // Static | Fullstack | Both

    // Host API schema/version range this adapter is known to be
    // compatible with. `pocopine deploy doctor` probes the host API
    // (e.g. `GET /v1/version` or a GraphQL introspection query) and
    // emits a Warn if the reported version falls outside this range —
    // a clear signal that an adapter PR is overdue.
    fn tested_against(&self) -> semver::VersionReq;

    // Pure: no I/O. Surfaces errors before we build a 200 MB image.
    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint>;

    // Pure: writes files to a staging dir. The orchestrator decides
    // whether to flush them, diff against existing, or just print
    // (for --dry-run).
    fn render_config(&self, spec: &DeploySpec, out: &mut StagedFiles);

    // I/O: invokes `docker build` (fullstack) or copies `dist/` (static).
    fn build_artefact(&self, spec: &DeploySpec) -> Result<Artefact>;

    // I/O: calls host API (HTTP/GraphQL), pushes image to host registry.
    // No host CLI shell-out. Streams progress via tracing.
    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> Result<DeployOutcome>;

    // Pure: returns follow-up commands the user must run once
    // (provisioning, secrets). The adapter never runs them.
    fn post_deploy_hint(&self, spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint>;
}

pub enum Constraint {
    Refuse(String),  // halts deploy
    Warn(String),    // proceed; user should know
    Hint(String),    // informational
}

pub enum Hint {
    OneTime(String), // one-time setup the user must run themselves
    Info(String),    // informational, e.g. deployment URL
}

pub enum Artefact {
    OciImage { tag: String },
    StaticDist { path: PathBuf },
}
```

`render_config` taking `&mut StagedFiles` means each adapter just
calls `out.write("railway.json", contents)`; no filesystem I/O
happens until the orchestrator flushes the staging dir. That's how
`--dry-run` shows everything without touching anything, and how the
test suite asserts on generated files without running `docker`.

### 5.3 The launcher

`pocopine-launcher` makes "one image, many processes" work uniformly:

```rust
fn main() -> ! {
    let proc = std::env::args().nth(1)
        .or_else(|| std::env::var("POCOPINE_PROCESS").ok())
        .expect("usage: pocopine-launcher <process>");

    let bin = match proc.as_str() {
        "web"    => "/usr/local/bin/server",
        "worker" => "/usr/local/bin/worker",
        other    => panic!("unknown process: {other}"),
    };

    let err = exec::Command::new(bin)
        .args(std::env::args().skip(2))
        .exec();
    panic!("exec failed: {err}");
}
```

The match arms are generated at build time from `[deploy.processes]`,
so adding a process type means a TOML change, not a launcher patch.
Each host invokes the entrypoint with a process name:

- Fly: `pocopine-launcher web` via `[processes.web]` in `fly.toml`.
- Railway: `pocopine-launcher web` in `startCommand` of the `web` service.
- Render: `dockerCommand: pocopine-launcher web` in `render.yaml`.
- Cloud Run: `args: ["web"]` in the service spec.

### 5.4 Same TOML → host outputs

Given the §4.1 example, the **Fly adapter** produces:

```dockerfile
# Dockerfile  (identical across fullstack hosts)
FROM rust:1.84-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release --bin server --bin worker --bin pocopine-launcher

FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/release/server            /usr/local/bin/server
COPY --from=build /app/target/release/worker            /usr/local/bin/worker
COPY --from=build /app/target/release/pocopine-launcher /usr/local/bin/pocopine-launcher
COPY --from=build /app/dist                             /var/lib/pocopine/dist
ENV  POCOPINE_DIST=/var/lib/pocopine/dist  LOG_LEVEL=info
ENTRYPOINT ["pocopine-launcher"]
```

```toml
# fly.toml
app             = "my-app"
primary_region  = "fra"

[build]
dockerfile = "Dockerfile"

[processes]
web    = "web"
worker = "worker"

[[services]]
processes      = ["web"]
internal_port  = 8080
auto_start     = true
auto_stop      = "suspend"

  [[services.http_checks]]
  path = "/healthz"

[[vm]]
processes = ["web"];    size = "shared-cpu-1x"; memory = "512mb"
[[vm]]
processes = ["worker"]; size = "shared-cpu-1x"; memory = "256mb"
```

The **Railway adapter** produces the *same Dockerfile* plus:

```json
// railway.json
{
  "$schema": "https://railway.app/railway.schema.json",
  "build": { "builder": "DOCKERFILE", "dockerfilePath": "Dockerfile" },
  "services": [
    { "name": "web",    "startCommand": "pocopine-launcher web",
      "healthcheckPath": "/healthz", "numReplicas": 1 },
    { "name": "worker", "startCommand": "pocopine-launcher worker",
      "numReplicas": 1 }
  ]
}
```

One Dockerfile, two host-config files, zero pocopine code in user
crates. Switching from Fly to Railway is `--target fly` →
`--target railway` plus a one-time `railway add postgresql` +
`railway add redis` to provision the services pocopine doesn't own.

### 5.5 Who does what

| Step | Who | Frequency |
|---|---|---|
| Author `[deploy]` block | user | once |
| Install Rust toolchain (rustup) | user | once — required for the default local build |
| Install Docker | user | once — **only if** using `pocopine build --container` or shipping to a registry-backed host |
| Obtain host API token from dashboard | user | once per host |
| `pocopine deploy auth <host>` — paste token | user | once per host |
| Create app on host | **adapter calls host API** | once per host |
| Provision Postgres add-on | **adapter calls host API** | once per host |
| Provision Redis add-on | **adapter calls host API** | once per host |
| Set host-side env / secrets | **adapter calls host API** | every deploy |
| Build OCI image (`docker build`) | adapter | every deploy |
| Push image to host registry | adapter | every deploy |
| Render `fly.toml` / `railway.json` (audit artefact) | adapter | every deploy (idempotent) |
| Trigger deployment | adapter → host API | every deploy |

`pocopine deploy auth <host>` is a one-time interactive flow per host
that prompts for the API token (linking the user to the dashboard URL
where the token is generated) and writes it to
`~/.pocopine/credentials.toml` with `0600` perms. Tokens can also be
provided via env vars (`POCOPINE_FLY_TOKEN`, `POCOPINE_RAILWAY_TOKEN`,
…) for CI use; env-var values take precedence over the file.

Pocopine **does** call host APIs to create apps, provision add-ons,
set env vars, push images, and trigger deployments. What it still
doesn't do is operate the infrastructure: we're calling the host's
managed-service APIs, not standing up a database ourselves. The
boundary in §2.3 holds — we're at the app layer, just talking to the
host over HTTP instead of over the host's CLI shim.

### 5.6 `detect_constraints` is where portability actually lives

Most of the adapter's value lives in this method: it's where
mismatched specs and hosts get caught *before* we build an image.
Two per-adapter examples:

```rust
// Fly adapter
fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint> {
    let mut out = vec![];
    if spec.has_process("worker") && self.is_static_only() {
        out.push(Constraint::Refuse(
            "static-only host can't run a `worker` process".into()
        ));
    }
    if spec.process("web").scale.min == 0 {
        out.push(Constraint::Hint(
            "scale-to-zero enabled on `web` via fly auto_stop=suspend".into()
        ));
    }
    if spec.requires_redis() {
        out.push(Constraint::Hint(
            "Fly has no first-party Redis; using Upstash partner add-on".into()
        ));
    }
    out
}

// CF Pages adapter (static-only)
fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint> {
    let mut out = vec![];
    if spec.mode == Mode::Fullstack {
        out.push(Constraint::Refuse("CF Pages is static-only".into()));
    }
    if spec.uses_collab {
        out.push(Constraint::Refuse(
            "collab requires WebSocket; use Fly / Railway / Render".into()
        ));
    }
    out
}
```

`Refuse` halts the deploy with a concrete reason. `Warn` and `Hint`
print and proceed. This is the layer that makes switching hosts safe:
an incompatible spec fails at the CLI before anything builds, with a
message naming the specific process or feature that doesn't fit.

## 6. Per-target mapping

Collab is omitted from the table: it always rides inside `web`. Hosts
that need a hint (e.g. disabling sticky sessions, allowing WebSocket
upgrades) get it from the `uses_collab` flag described in §9.

| Host | `web` | `worker` | Postgres | Redis | Config files generated |
|---|---|---|---|---|---|
| **Fly.io** | `[processes.web]` (sticky off if collab) | `[processes.worker]` | Fly Postgres or external | Upstash partner | `Dockerfile`, `fly.toml`, `.dockerignore` |
| **Railway** | service | service | Railway Postgres | Railway Redis | `railway.json`, optional `Dockerfile` (Nixpacks otherwise) |
| **Render** | web service (WS-enabled if collab) | background worker | Render Postgres | Render Redis | `render.yaml`, `Dockerfile` |
| **Cloud Run** | service (HTTP/2 if collab) | Cloud Run Job *or* service | Cloud SQL | Memorystore | `Dockerfile`, `service.yaml`, `job.yaml` |
| **CF Pages** | static | ✗ refused | n/a | n/a | none |
| **Netlify** | static | ✗ refused | n/a | n/a | `_headers`, `netlify.toml` |
| **GH Pages** | static | ✗ refused | n/a | n/a | service worker, `pages.yml` |

`pocopine deploy --target gh-pages` against a project that declares a
`worker` process fails at `detect_constraints` with:

```
error: target `gh-pages` is static-only; this project declares a `worker` process.
       compatible full-stack targets: fly, railway, render, cloud-run.
```

## 7. CLI surface

```
pocopine build                           # local Rust toolchain (default); output to target/ + dist/
pocopine build --container               # opt-in: build inside ghcr.io/pocopine/build:<v>; runtime = docker
pocopine build --container=docker        # explicit (same as --container)
pocopine build --container=podman        # planned (post-v1)
pocopine build --image-only              # only emit the runtime OCI image; skip the static dist/
pocopine build --static-only             # only emit dist/; skip the runtime image

pocopine deploy                          # prompt for target on first run; remember in .pocopine/deploy/last.toml
pocopine deploy --target fly             # build (if stale) + render config + ship via host API
pocopine deploy --target fly --dry-run   # print rendered config + planned API calls, do nothing
pocopine deploy --target fly --prod      # production environment (default: staging where the host supports it)
pocopine deploy --target fly --skip-build  # ship the existing artefact (CI pattern: build once, deploy to many)

pocopine deploy auth <host>              # one-time: paste API token from host dashboard → ~/.pocopine/credentials.toml
pocopine deploy auth --list              # list configured hosts and token freshness
pocopine deploy auth --revoke <host>     # delete a stored token

pocopine deploy init --target fly        # scaffold host files for a target you haven't used
pocopine deploy diff --target fly        # diff currently-deployed config against what would be regenerated
pocopine deploy logs --target fly        # tail host logs (host API; SSE/WebSocket streaming → pocopine.log)

pocopine deploy doctor                   # validate Pocopine.toml [deploy], probe host API versions, check token freshness, print fixes; check build-container image is up to date
```

`.pocopine/deploy/<target>.toml` records the host-side identifiers
(app name, project ID) so the host CLI doesn't re-prompt on every run.
That file is the only thing pocopine owns per-target; everything else
(Dockerfile, fly.toml, render.yaml) is regenerated each deploy from
`Pocopine.toml`. Users who hand-edit a generated file are warned on the
next deploy that their edit will be overwritten. Two escape hatches:

- **`[deploy.<host>]` override block** — selective adjustments for fields
  the portable contract doesn't expose. The host file still regenerates
  on every deploy; overrides merge in.
- **`[deploy.<host>] generated = "freeze"`** — stop regenerating the host
  config file entirely. Pocopine treats the existing `fly.toml` /
  `render.yaml` / `railway.json` as authoritative and only rebuilds the
  image + invokes the host CLI. This is the explicit pin-to-known-good
  path when an adapter lags upstream or the user needs a host feature
  pocopine hasn't wrapped yet. `pocopine deploy doctor` lists frozen
  targets so users don't forget the file is no longer generated.

## 8. Single-process escape hatch

For hobby apps that don't need horizontal scale, the contract collapses
to a single process and no Redis:

```toml
[deploy]
mode = "fullstack"

[deploy.processes.web]
bin  = "server"
port = 8080
scale = { min = 1, max = 1 }

[deploy.services.postgres]
required = true
```

If a `#[job]` exists in the project and no `worker` process is
declared, `pocopine deploy doctor` recommends one of:

1. Declare a `worker` process and a `redis` service (multi-process production).
2. Set `POCOPINE_JOB_BACKEND=memory` in `[deploy.env]` (single-process; jobs run inside `web`).

Option (2) is the Shuttle-replacement path: one Fly Machine, one
Postgres, no Redis bill. RFC 067's memory backend already supports
this; the deploy contract just surfaces it as a deliberate choice.

Collab does not collapse separately because it already lives in `web`.
The single-process app with `pocopine-collab` linked still hosts the
WebSocket handler inside `web`; what does collapse is **Redis** — with
only one `web` replica and no `worker`, the in-memory Redis substitute
provided by `pocopine-collab` for tests becomes a viable production
choice. The CLI prints a warning that this mode does not scale
horizontally (collab Pub/Sub fan-out stops at the process boundary).

## 9. Detection and gating

The CLI inspects the *built artefact's metadata* (a small JSON sidecar
emitted by `pocopine build`) to infer what the app actually uses:

- `uses_jobs: bool` — true if any `#[job]` macro expanded.
- `uses_collab: bool` — true if `pocopine-collab` is in the dependency graph.
- `uses_storage: bool` — true if any `#[server]` function touches pocopine's storage layer.
- `uses_websocket: bool` — true if the app declares a WebSocket route.

These flags drive automatic inference of required services and
target-compatibility refusals. Users can override with explicit
`required = false` if they're bringing their own infra (e.g. an
external Upstash account not provisioned by the host).

## 10. Implementation plan

Four phases, each independently mergeable, ordered by user demand and
contract dependency.

**Phase 1 — Build container + contract + launcher + Fly adapter.**
- Publish `ghcr.io/pocopine/build:<version>` (see §4.4).
- `[deploy]` schema parsing in `pocopine-cli`.
- `pocopine-launcher` shim crate.
- OCI image builder (multi-stage Dockerfile generation + `docker build`).
- Fly adapter: `Dockerfile`, `fly.toml` with `[processes]`, Fly machines API + Fly Postgres + Upstash Redis partner.
- `pocopine deploy doctor` + `--dry-run`.

**Phase 2 — Railway + Render adapters.**
- Railway adapter: `railway.json` per service; `railway up`. Appendix A is the prototype.
- Render adapter: `render.yaml`; `render deploy` or git push.
- Both reuse Phase 1's OCI image; the only delta is config generation.

**Phase 3 — Static adapters.**
- CF Pages, Netlify, GH Pages (Vercel optional).
- Static-mode artefact: existing `pocopine build` output to `dist/`.
- Detection-driven refusal for incompatible projects (per §9).

**Phase 4 — Cloud Run (or first community-requested host).**
- Workers as Cloud Run Jobs (the awkward one — worth its own design discussion).
- Cloud SQL / Memorystore wiring.

Per adapter target: ~200–400 LOC of Rust, mostly file generation + shell-out. Each phase ships with at least one `examples/` app deployed end-to-end so we dogfood the path.

## 11. Open questions

1. **Image registry.** Fly pushes to its own registry; Render builds
   from git; Railway can do either; Cloud Run pulls from Artifact
   Registry / Docker Hub. Should pocopine push to a user-owned registry
   (one source of truth, slower deploys) or let each adapter use the
   host's default (faster, less portable)? **Proposal:** host default
   in v1; introduce `[deploy.registry]` block when a second user asks.

2. **Worker process scale-to-zero.** Fly supports it natively; Railway
   doesn't (worker stays warm); Render doesn't for background workers.
   The contract's `scale = { min = 0 }` is honoured where possible and
   silently treated as `min = 1` elsewhere — with a warning in `doctor`.
   Is silent floor acceptable, or do we refuse the deploy? **Proposal:** floor + warning.

3. **Collab sticky sessions.** RFC 073 mandates stateless WebSocket
   servers (Redis is the coordination layer). Because collab rides
   inside `web`, sticky sessions on `web` would be harmless but
   wasteful. When `uses_collab` is true the adapter sets sticky off on
   `web` for every host that exposes the knob. No open issue, just
   calling it out.

4. **Migrations.** `sqlx migrate` / `diesel migration run` needs to
   happen on deploy, ideally as a release-time job. Heroku has release
   commands; Fly has release-command in `fly.toml`; Railway has
   pre-deploy commands; Render has pre-deploy. **Proposal:** add
   `[deploy.release]` with a `command = ["pocopine", "migrate"]` field
   in a follow-up RFC; out of scope here.

5. **Per-environment overrides** (staging vs production). Heroku-style
   review apps require multiple environments. **Proposal:** `[deploy.production]`
   / `[deploy.staging]` sub-tables that override the base block;
   default environment names per host; design in a follow-up.

6. **What about no-CLI hosts** (push-to-deploy via git, e.g. Render's
   git mode)? Those adapters skip the `deploy()` step and instead
   commit the generated host config to a deploy branch. Compatible
   with the trait, but the UX is different — worth a dedicated section
   if Render's git mode is preferred over its API.

7. **Adapter freshness and keep-up cost.** Hosts evolve their APIs:
   endpoints get added, fields rename, schemas version. Talking to
   APIs directly (rather than wrapping a host CLI that absorbs churn)
   means we're closer to the metal — drift hits us sooner. The
   mitigations baked into the contract are: (a) every adapter declares
   a `tested_against` schema range for the host's API (§5.2), and
   `pocopine deploy doctor` probes the API's version endpoint and
   warns when it falls outside; (b) the `[deploy.<host>] generated =
   "freeze"` escape hatch (§7) lets users pin to a known-good host
   config while waiting for an adapter update; (c) where a host
   publishes an OpenAPI / GraphQL schema, the API-client layer is
   codegen'd from the schema, so much of the keep-up is mechanical.
   Adapter updates themselves are **community-PR-driven**: we do not
   staff a vendor-tracking team. Active users of each host hit drift
   first and PR the fix. **Proposal:** accept this model and document
   it here; revisit only if a single host's adapter goes stale for >2
   months at a stretch.

## 12. Alternatives considered

- **Keep RFC 041's "thin shell-out wrappers."** Rejected: workers + collab require describing a process graph and backing services, which means *some* contract has to exist. Hiding it inside bash scripts that vary per host is worse than naming it.
- **Adopt a Nomad-style job spec.** Powerful but overkill — Nomad's model is for orchestrating long-running services across a cluster you own. Pocopine apps target managed hosts that already do orchestration.
- **Adopt `docker-compose.yml` as the portable spec.** Tempting (everyone knows it), but compose semantics around volumes, networks, and depends-on don't map cleanly to Fly's machines or Railway's services. Worse, compose encourages users to think they can `docker-compose up` to a host that doesn't offer compose — they can't.
- **Build a `pocopine-cloud` managed service.** Rejected (same as RFC 041 §9): pocopine should not operate infrastructure.
- **Pick one host and bless it (Fly)** — like RFC 041 picked Shuttle. Rejected: the entire point of this RFC is that betting on a single vendor is what got us here.

## 13. What changed vs. RFC 041

| Topic | RFC 041 | RFC 080 |
|---|---|---|
| Flagship full-stack target | Shuttle.rs (Rust-native, no Docker) | Fly.io (OCI-based, multi-process) |
| Pocopine's role | "thin shell-out wrappers, zero deploy logic" | owns a portable contract + adapter trait |
| Host integration | shells out to host CLIs (`flyctl`, `railway`, `wrangler`, …) | calls host HTTP/GraphQL APIs directly; only `docker` is a required local binary |
| Build environment | assumes local Rust toolchain | local toolchain by default; opt-in canonical build container via `--container[=docker\|podman]` |
| Artefact | implied: per-host build | one OCI image (fullstack) or `dist/` (static) |
| Process model | implicit single binary | explicit two-process graph (`web` hosts collab; `worker` runs jobs) |
| Backing services | not modelled | first-class `postgres` / `redis` declarations |
| Adapter shape | bash + host CLI | Rust trait with pure `render_config` + API client |
| Vendor lock-in stance | encapsulation (Shuttle macro generated by us) | zero vendor code in user crates or build output |
| Static targets | unchanged | unchanged (CF Pages, Netlify, GH Pages, Vercel) |
| Out-of-scope additions | — | infra provisioning, IaC, secrets stores |

RFC 041 is marked **Superseded** by this RFC. Its target list and host
analyses remain useful as background; its CLI surface and "wrapper"
framing are replaced wholesale.

## Appendix A — Railway adapter prototype

Illustrative implementation. Types referenced (`DeploySpec`,
`StagedFiles`, `Constraint`, etc.) come from the `pocopine-deploy`
crate scaffolding that lands in Phase 1 (§10). This file would live
at `crates/pocopine-deploy/src/adapters/railway.rs` and is the
template Phase 2 follows.

The prototype talks to Railway's GraphQL API directly — no `railway`
CLI required. Field names (`projectUpsert`, `serviceUpsert`,
`deploymentCreate`, `pluginEnsure`) follow the shape of Railway's
public schema; a real implementation pins exact names against the
introspection result at the version declared in `tested_against`.

```rust
//! Railway adapter (RFC 080 §5).
//!
//! Talks to Railway's GraphQL API directly — no `railway` CLI required.
//! Builds the OCI image locally via `docker build`, pushes to Railway's
//! registry using the token stashed by `pocopine deploy auth railway`,
//! then triggers a deployment per service via GraphQL mutations.

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Command;
use tracing::info;

use crate::{
    common, credentials, Artefact, AdapterMode, Constraint, DeployAdapter, DeployOutcome,
    DeploySpec, Hint, Mode, StagedFiles,
};

const RAILWAY_GRAPHQL: &str = "https://backboard.railway.app/graphql/v2";
const RAILWAY_REGISTRY: &str = "registry.railway.app";

pub struct RailwayAdapter;

impl DeployAdapter for RailwayAdapter {
    fn name(&self) -> &'static str { "railway" }
    fn mode(&self) -> AdapterMode { AdapterMode::Fullstack }

    fn tested_against(&self) -> semver::VersionReq {
        // Railway's public GraphQL is "v2"; we track schema generations
        // as semver majors. `doctor` issues an introspection query and
        // warns when the reported generation falls outside this range.
        semver::VersionReq::parse(">=2.0.0, <3.0.0").expect("static range parses")
    }

    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint> {
        let mut out = vec![];

        if spec.mode == Mode::Static {
            out.push(Constraint::Refuse(
                "railway is fullstack-only; use `cf-pages` for static apps".into(),
            ));
        }

        if credentials::load("railway").is_err() {
            out.push(Constraint::Refuse(
                "no railway token. Run `pocopine deploy auth railway` first \
                 (token from https://railway.app/account/tokens).".into(),
            ));
        }

        // Railway holds non-web services warm. scale.min = 0 is silently
        // floored to 1 (§11 open question 2).
        for (name, proc) in spec.processes() {
            if !proc.is_public() && proc.scale.min == 0 {
                out.push(Constraint::Warn(format!(
                    "railway holds non-web services warm; flooring `{name}` min to 1"
                )));
            }
        }

        if spec.requires_redis() && !spec.has_secret("REDIS_URL") {
            out.push(Constraint::Hint(
                "railway managed redis will be provisioned on first deploy".into(),
            ));
        }
        if spec.requires_postgres() && !spec.has_secret("DATABASE_URL") {
            out.push(Constraint::Hint(
                "railway managed postgres will be provisioned on first deploy".into(),
            ));
        }

        out
    }

    fn render_config(&self, spec: &DeploySpec, out: &mut StagedFiles) {
        // `railway.json` is no longer how we ship the spec — that goes
        // over the API. But we still write it as an audit artefact for
        // `pocopine deploy diff` so users can see what we sent.
        out.write("Dockerfile",     common::render_dockerfile(spec));
        out.write(".dockerignore",  common::DOCKERIGNORE);
        out.write("railway.json",   render_railway_json(spec));
    }

    fn build_artefact(&self, spec: &DeploySpec) -> Result<Artefact> {
        // Tag against Railway's registry directly so `docker push` later
        // doesn't need a retag step. The token loaded in `deploy()` is
        // used for both `docker login` (via a credential helper) and
        // the GraphQL mutations.
        let tag = format!(
            "{}/{}/{}:{}",
            RAILWAY_REGISTRY, spec.app_name, spec.app_name, spec.git_sha,
        );
        info!(target: "pocopine.log", image = %tag, "building OCI image");

        let status = Command::new("docker")
            .args(["build", "-t", &tag, "."])
            .status()
            .context("invoking `docker build`")?;
        if !status.success() {
            bail!("docker build failed");
        }
        Ok(Artefact::OciImage { tag })
    }

    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> Result<DeployOutcome> {
        let token = credentials::load("railway")
            .context("no railway token; run `pocopine deploy auth railway`")?;
        let client = RailwayClient::new(&token);

        // 1. Resolve or create the project.
        let project_id = client.upsert_project(&spec.app_name)?;
        info!(target: "pocopine.log", project = %project_id, "resolved project");

        // 2. Push the locally-built image to Railway's registry.
        let Artefact::OciImage { tag } = artefact else {
            bail!("railway adapter requires an OCI image artefact");
        };
        common::docker_login(RAILWAY_REGISTRY, "railway", &token)?;
        info!(target: "pocopine.log", image = %tag, "pushing image");
        let status = Command::new("docker").args(["push", tag]).status()?;
        if !status.success() { bail!("docker push failed"); }

        // 3. Ensure backing services exist (idempotent on the server side).
        if spec.requires_postgres() {
            client.ensure_addon(&project_id, "POSTGRESQL")?;
        }
        if spec.requires_redis() {
            client.ensure_addon(&project_id, "REDIS")?;
        }

        // 4. Upsert one service per process and trigger a deployment.
        for (proc_name, proc) in spec.processes() {
            let svc = client.upsert_service(&project_id, proc_name, &ServiceConfig {
                image:            tag.clone(),
                start_command:    format!("pocopine-launcher {proc_name}"),
                healthcheck_path: proc.healthcheck.clone(),
                num_replicas:     proc.scale.min.max(1),
                envs:             spec.env_for_service(proc_name),
            })?;
            let deployment = client.trigger_deployment(&svc.id)?;
            info!(
                target: "pocopine.log",
                service = %proc_name, deployment = %deployment.id,
                "deployment triggered",
            );
        }

        Ok(DeployOutcome {
            url: format!("https://{}.up.railway.app", spec.app_name),
            host_ids: vec![project_id],
        })
    }

    fn post_deploy_hint(&self, _spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint> {
        // No more "run these CLI commands" — the API did all of it.
        vec![Hint::Info(format!("deployed to {}", outcome.url))]
    }
}

// ─── Railway GraphQL client (illustrative shape) ───────────────────────────

struct RailwayClient { http: Client, token: String }

impl RailwayClient {
    fn new(token: &str) -> Self {
        Self { http: Client::new(), token: token.to_owned() }
    }

    fn graphql<T: for<'de> Deserialize<'de>>(
        &self, query: &str, variables: serde_json::Value,
    ) -> Result<T> {
        #[derive(Deserialize)]
        struct GqlResponse<T> { data: Option<T>, errors: Option<Vec<GqlError>> }
        #[derive(Deserialize, Debug)]
        struct GqlError { message: String }

        let resp: GqlResponse<T> = self.http.post(RAILWAY_GRAPHQL)
            .bearer_auth(&self.token)
            .json(&json!({ "query": query, "variables": variables }))
            .send()?
            .error_for_status()?
            .json()?;

        if let Some(errors) = resp.errors {
            bail!(
                "railway graphql: {}",
                errors.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("; "),
            );
        }
        resp.data.context("railway returned no data")
    }

    fn upsert_project(&self, name: &str) -> Result<String> {
        #[derive(Deserialize)] struct Resp { project_upsert: ProjectInfo }
        #[derive(Deserialize)] struct ProjectInfo { id: String }
        let r: Resp = self.graphql(
            r#"mutation($name: String!) {
                 projectUpsert(name: $name) { id }
               }"#,
            json!({ "name": name }),
        )?;
        Ok(r.project_upsert.id)
    }

    fn upsert_service(&self, project_id: &str, name: &str, cfg: &ServiceConfig) -> Result<ServiceInfo> {
        #[derive(Deserialize)] struct Resp { service_upsert: ServiceInfo }
        let r: Resp = self.graphql(
            r#"mutation($p: ID!, $n: String!, $c: ServiceInput!) {
                 serviceUpsert(projectId: $p, name: $n, config: $c) { id name }
               }"#,
            json!({ "p": project_id, "n": name, "c": cfg }),
        )?;
        Ok(r.service_upsert)
    }

    fn trigger_deployment(&self, service_id: &str) -> Result<DeploymentInfo> {
        #[derive(Deserialize)] struct Resp { deployment_create: DeploymentInfo }
        let r: Resp = self.graphql(
            r#"mutation($s: ID!) {
                 deploymentCreate(serviceId: $s) { id url }
               }"#,
            json!({ "s": service_id }),
        )?;
        Ok(r.deployment_create)
    }

    fn ensure_addon(&self, project_id: &str, kind: &str) -> Result<()> {
        // Server-side idempotency: returns existing plugin if one already
        // matches `kind` on this project, otherwise creates one.
        let _: serde_json::Value = self.graphql(
            r#"mutation($p: ID!, $k: PluginKind!) {
                 pluginEnsure(projectId: $p, kind: $k) { id }
               }"#,
            json!({ "p": project_id, "k": kind }),
        )?;
        Ok(())
    }
}

#[derive(Serialize)]
struct ServiceConfig {
    image: String,
    start_command: String,
    healthcheck_path: Option<String>,
    num_replicas: u32,
    envs: Vec<(String, String)>,
}
#[derive(Deserialize)] struct ServiceInfo { id: String, name: String }
#[derive(Deserialize)] struct DeploymentInfo { id: String, url: String }

// ─── Audit-only config renderer (for `pocopine deploy diff`) ───────────────

fn render_railway_json(spec: &DeploySpec) -> String {
    #[derive(Serialize)]
    struct Config<'a> {
        #[serde(rename = "$schema")]
        schema: &'static str,
        build: Build,
        services: Vec<Service<'a>>,
    }
    #[derive(Serialize)]
    struct Build {
        builder: &'static str,
        #[serde(rename = "dockerfilePath")]
        dockerfile_path: &'static str,
    }
    #[derive(Serialize)]
    struct Service<'a> {
        name: &'a str,
        #[serde(rename = "startCommand")]
        start_command: String,
        #[serde(rename = "healthcheckPath", skip_serializing_if = "Option::is_none")]
        healthcheck_path: Option<&'a str>,
        #[serde(rename = "numReplicas")]
        num_replicas: u32,
    }

    let services = spec
        .processes()
        .map(|(name, p)| Service {
            name,
            start_command:    format!("pocopine-launcher {name}"),
            healthcheck_path: p.healthcheck.as_deref(),
            num_replicas:     p.scale.min.max(1),
        })
        .collect();

    serde_json::to_string_pretty(&Config {
        schema: "https://railway.app/railway.schema.json",
        build: Build { builder: "DOCKERFILE", dockerfile_path: "Dockerfile" },
        services,
    })
    .expect("railway.json serialisation is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{make_spec_full, make_spec_static, make_spec_worker_scale_zero};

    #[test]
    fn render_railway_json_matches_snapshot() {
        let spec = make_spec_full();
        insta::assert_snapshot!(render_railway_json(&spec));
    }

    #[test]
    fn refuses_static_mode() {
        let spec = make_spec_static();
        let constraints = RailwayAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(c, Constraint::Refuse(_))));
    }

    #[test]
    fn warns_on_worker_scale_zero() {
        let spec = make_spec_worker_scale_zero();
        let constraints = RailwayAdapter.detect_constraints(&spec);
        assert!(constraints.iter().any(|c| matches!(c, Constraint::Warn(_))));
    }

    // Note: deploy() is HTTP-bound and tested separately via wiremock; see
    // crates/pocopine-deploy/tests/railway_http.rs.
}
```

### Notes on the prototype

- **API-direct, no CLI.** The only external binary invoked is `docker`
  (for `build`, `login`, `push`). Everything else — project lookup,
  service upsert, add-on provisioning, deployment trigger — is one
  GraphQL call. Users never install `railway`.
- **Token handling.** `credentials::load("railway")` reads
  `~/.pocopine/credentials.toml` (`0600`) or `$POCOPINE_RAILWAY_TOKEN`
  (env takes precedence for CI). `docker_login` uses the same token
  via a credential helper so registry auth doesn't ask twice.
- **Idempotency lives on the server.** `projectUpsert`, `serviceUpsert`,
  and `pluginEnsure` are designed to be safe to call repeatedly. The
  adapter does not maintain client-side state about "did we provision
  Redis already?" — it asks the server every deploy.
- **GraphQL field names are illustrative.** Real implementation pins
  to the exact schema reported by introspection at the version
  declared in `tested_against`. A `schema_gen` build script can
  codegen the client from `railway-introspection.json` so adapter
  drift is visible at compile time.
- **Logging.** Every API call and `docker` shell-out is announced via
  `tracing::info!(target: "pocopine.log", ...)` per RFC 069; no raw
  `println!` / `eprintln!`. Failed GraphQL responses include the
  Railway-side error messages verbatim.
- **HTTP tests use `wiremock`.** Snapshot tests cover the pure parts
  (`render_railway_json`, `detect_constraints`); the `deploy()` path
  hits a mocked GraphQL server in `tests/railway_http.rs` so we can
  assert on exact mutation payloads without a real Railway account.
- **What's still TODO for production.** Streaming build logs from the
  Railway API into `pocopine.log` (Railway pushes them over a
  WebSocket); retry-on-transient-failure with exponential backoff;
  handling 401 by prompting `pocopine deploy auth railway --refresh`;
  handling deploy-while-uncommitted with the `--dirty` flag from RFC 041 §8.3.
