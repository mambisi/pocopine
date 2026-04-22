# RFC 041 — `pocopine deploy` targets

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-22 |
| **Related** | [RFC 002 — App / Stores / Servers](./rfc-002-app-stores-servers.md), [RFC 037 — JS client module bridge (draft)](./rfc-037-js-bridge.md) |

## 1. Summary

Ship a `pocopine deploy --target <name>` subcommand that wraps
the deploy CLIs of the hosts pocopine users are most likely to
reach for. Two modes, eight targets total, ranked by priority:

**Full-stack** (serves client bundle + `#[server]` POST endpoints
via `pocopine-server`):
1. **Shuttle.rs** — first-class. Rust-native DX, single cargo
   command, managed Postgres via annotations.
2. **Fly.io** — second-class. Docker-based; multi-region; micro-VMs.
3. **Railway** — third. Nixpacks auto-detection; predictable $5/mo.

**Static-only** (client bundle served as static assets, no
server-side Rust; `#[server]` functions unavailable):
1. **Cloudflare Pages** — unlimited bandwidth, generous free tier.
2. **Netlify** — popular, 100 GB/mo bandwidth ceiling on free.
3. **Firebase Hosting** — useful when the JS bridge (RFC 037)
   already pulls in a Firebase SDK — one vendor, one console.
4. **GitHub Pages** — zero-setup free; WASM MIME workaround required.
5. **Vercel** — works but Node-first; WASM is second-class.

`pocopine deploy` is a thin forwarder: each target is one bash
adapter that bundles the artefacts then calls the host's own CLI
(`cargo shuttle deploy`, `flyctl deploy`, `railway up`, `wrangler
pages deploy`, `netlify deploy`, `firebase deploy`, `gh-pages`,
`vercel deploy`). Pocopine owns zero deployment logic of its own.

## 2. Motivation

Right now, shipping a pocopine app is "figure out your target,
read their quickstart, write your own build + upload script."
Every user does the same 30-minute evaluation. The concrete
friction:

- Different builds per mode. Static mode is just
  `cargo build --target wasm32-unknown-unknown --release` +
  `wasm-bindgen` + the static asset copy. Server mode is
  `cargo build --release` of the `pocopine-server` binary plus
  the same static assets. Users have to know which commands
  produce what.
- WASM MIME-type trap. Several hosts (GitHub Pages, plain S3)
  serve `.wasm` with the wrong `Content-Type`, which Firefox
  and Safari refuse on strict loads. A wrapper can detect this
  per-target and inject a headers file.
- Brotli / gzip pre-compression. Cloudflare does it for you;
  others need the file committed. A wrapper can emit the
  compressed variants at build time.
- `pocopine-server` needs a Dockerfile for Fly / Railway / any
  non-Shuttle target. We can generate a reasonable default
  (multi-stage build, distroless runtime) and let the user
  override.

Net: we spend one afternoon per adapter and save every user the
same afternoon. No ecosystem lock-in — the adapters are
shell-out wrappers, not frameworks.

## 3. CLI surface

```
pocopine deploy                          # asks: pick a target
pocopine deploy --target shuttle         # explicit
pocopine deploy --target shuttle --prod
pocopine deploy --target fly --region fra
pocopine deploy --target cf-pages
pocopine deploy --target netlify --site my-site-id
pocopine deploy --target firebase
pocopine deploy --target gh-pages
pocopine deploy --target vercel
pocopine deploy --target railway
```

First run per target writes a small `.pocopine/deploy/<target>.toml`
that pins config the host CLI would otherwise nag for every run
(site id, project name, region, etc.). Everything else is
inherited from the host's own config file (`Shuttle.toml`,
`fly.toml`, `railway.json`, etc.) — pocopine doesn't own
deployment config, it just remembers where your project lives.

`pocopine deploy init --target <name>` scaffolds the minimal
config for a target the user hasn't used before (Dockerfile for
Fly / Railway, `firebase.json` for Firebase, `_headers` for
Netlify, etc.).

## 4. Target-by-target

### 4.1 Shuttle.rs (full-stack, first-class)

Adapter: wraps `cargo shuttle deploy`. Users' `#[server]`
functions and the pocopine-server binary become a single Shuttle
service via `#[shuttle_runtime::main]`. The macro rewrite is the
only pocopine-side change (generate a Shuttle entrypoint when
the user opts in via `[package.metadata.pocopine] deploy.shuttle
= true` — or emit it unconditionally and let Shuttle's build
pick it up only when that's the deploy target).

Pros:
- Single cargo command. No Dockerfile. No YAML.
- Rust-native: no Node runtime in the deploy path.
- Managed Postgres via `#[shuttle_shared_db::Postgres]`. Users
  annotate their server functions and a database appears.
- Small permanent free tier.

Cons:
- Vendor-specific annotations bleed into user code. We
  encapsulate them so pocopine apps stay portable — the
  `#[shuttle_runtime::main]` entrypoint is generated from
  `pocopine-server`'s existing axum setup, not hand-written.
- Single-region by default. Users with multi-region needs drift
  to Fly.

### 4.2 Fly.io (full-stack)

Adapter: generates a `Dockerfile` (multi-stage, Rust 1.x-slim
build → distroless runtime) and a `fly.toml`, then
`flyctl deploy`.

Pros:
- Docker-based. Runs pocopine-server binary as-is.
- Multi-region. Micro-VMs spin up fast.
- Scale-to-zero supported — idle apps cost nothing.

Cons:
- No permanent free tier (trial only since 2024); small apps
  land at ~$2–5/mo.
- Users who don't know Docker hit a learning wall on
  customisation. Our default Dockerfile covers the 90% case.

### 4.3 Railway (full-stack)

Adapter: wraps `railway up`. Railway's Nixpacks auto-detects a
Rust project, so no Dockerfile is required; we only need to
point it at the `pocopine-server` binary's target in
`Cargo.toml`.

Pros:
- Zero-config Rust deploys. Auto-caching of
  `target/release/`.
- Predictable $5/mo Hobby plan.
- Clean UI for env vars / logs / rollbacks.

Cons:
- No permanent free tier; $5 trial credit only.
- Single-region per project.

### 4.4 Cloudflare Pages (static)

Adapter: wraps `wrangler pages deploy dist/`. Builds the
client bundle via the existing `pocopine build --release`
path, copies to `dist/`, uploads.

Pros:
- **Unlimited bandwidth** on the free tier — the deciding
  factor for a WASM app where the initial payload is
  hundreds of KB to a few MB.
- Automatic Brotli + correct WASM MIME.
- Preview URL per PR for free.
- Custom domains unlimited.

Cons:
- Build minutes capped at 500/mo (generous; not a constraint
  for most projects).
- Requires `wrangler` CLI (Node package); one dep in the
  deploy path.

### 4.5 Netlify (static)

Adapter: wraps `netlify deploy --prod --dir dist`. Emits a
`_headers` file so WASM gets `application/wasm` correctly.

Pros:
- Mature tooling. Preview URLs + form handling + redirects.

Cons:
- 100 GB/mo bandwidth ceiling on free — a WASM app can burn
  through that faster than static-HTML apps because every
  first visit downloads the whole bundle.

### 4.6 Firebase Hosting (static)

Adapter: wraps `firebase deploy --only hosting`. Emits
`firebase.json` with correct headers for `.wasm`.

Positioning: this target only earns its slot because users
who adopt Firebase Auth / Firestore via RFC 037's JS bridge
end up with the Firebase CLI in their dev environment
anyway. Deploying to the same console keeps everything in
one place.

Pros:
- Zero net dependency cost for users already on Firebase.
- Preview channels.
- Integrates with Firebase project auth rules, functions,
  etc.

Cons:
- 10 GB/mo bandwidth + 360 MB/day on Spark (free) plan.
- If you're not using other Firebase products, no reason to
  pick this over Cloudflare Pages.

### 4.7 GitHub Pages (static)

Adapter: builds `dist/`, writes to a `gh-pages` branch,
commits + pushes (or uses the new `deploy-pages` action via
workflow dispatch).

Pros:
- Zero cost, zero extra account.
- Ships a WASM MIME workaround via a service worker (generated
  at build time) since GitHub Pages doesn't let us set
  headers.

Cons:
- 1 GB site size limit, 100 GB/mo soft bandwidth cap.
- No custom headers: the WASM MIME problem is real — the
  service-worker workaround adds ~4 KB to the bundle.
- Builds through GitHub Actions; users who aren't on GitHub
  obviously can't use this.

### 4.8 Vercel (static)

Adapter: wraps `vercel deploy --prebuilt`. WASM MIME handled
via `vercel.json` headers.

Positioning: Vercel works, but its sweet spot is Next.js /
Node serverless. For a pure WASM client, Cloudflare Pages
wins on every dimension except "we already have a Vercel
account." Kept for users in that position.

## 5. What `pocopine-cli` generates

Per target, the adapter can synthesise:

- **Shuttle.rs**: `Shuttle.toml` + a `#[shuttle_runtime::main]`
  entry file that wires the user's existing axum router.
- **Fly.io**: `Dockerfile` (multi-stage Rust build, distroless
  runtime), `fly.toml`, `.dockerignore`.
- **Railway**: Optional `railway.json` + env-var scaffold.
- **Cloudflare Pages**: no extra files; `wrangler pages deploy`
  takes the dist directly.
- **Netlify**: `_headers` (WASM MIME) + `netlify.toml`.
- **Firebase**: `firebase.json` (hosting block) + `.firebaserc`.
- **GitHub Pages**: `.github/workflows/pages.yml` + a
  `service-worker.js` for WASM MIME workaround.
- **Vercel**: `vercel.json` with WASM MIME + rewrites.

All files generated to the project root or a `.pocopine/deploy/`
subdir. Users can customise; pocopine treats the file as
theirs once it exists.

## 6. Not in scope

- **Cloudflare Workers for server mode.** Requires `#[server]`
  to emit worker-rs-compatible handlers instead of axum.
  Separate RFC, big undertaking — left for later.
- **AWS / GCP / Azure native deploys (Lambda, Cloud Run, App
  Runner).** Docker users can already use Fly / Railway; a
  dedicated AWS adapter can follow if demand shows up.
- **Self-hosted / VPS** (Hetzner, DigitalOcean droplets,
  bare metal). Users in that camp aren't reaching for a
  deploy CLI; they're writing their own deploy scripts.
- **Database provisioning** beyond what Shuttle gives for free.
  Pocopine doesn't wrap Supabase / Neon / Turso provisioning;
  users add their DB URL to env vars like on any other host.

## 7. Implementation plan

Three PRs, independently mergeable, priority by user demand:

**PR 1 — Shuttle + Cloudflare Pages** (the two flagship targets;
covers full-stack first-class + static first-class).
- `pocopine deploy --target shuttle` + `--target cf-pages`.
- Shuttle entrypoint generator.
- Static-bundle builder (reuses the existing `pocopine build`
  path — just copies to `dist/` and calls `wrangler`).

**PR 2 — Fly.io + Railway + Netlify.**
- Dockerfile generator for Fly/Railway (one shared template).
- Netlify `_headers` emitter.

**PR 3 — Firebase + GitHub Pages + Vercel.**
- Firebase config generator.
- GitHub Pages service-worker MIME workaround.
- Vercel `vercel.json` emitter.

Per adapter: roughly 100–200 LOC of Rust inside
`pocopine-cli`, mostly file-generation + shell-out. Each
follow-up PR should land with at least one example in
`examples/` deployed to the target, so we dogfood the path.

## 8. Open questions

1. **Config file ownership.** When the user has already edited
   a `Dockerfile` / `fly.toml` / `netlify.toml`, does
   `pocopine deploy init --target <x> --force` overwrite?
   Proposal: **no, never overwrite**; only generate when the
   file is absent. Print a diff + instructions when outdated.

2. **Secrets handling.** Env vars for `#[server]`-backing APIs
   (Stripe keys, Firebase admin creds) live in the host's own
   secret store (`fly secrets set`, `shuttle secrets set`,
   etc.). Proposal: pocopine never touches secrets — the
   adapter prints the host's command on first deploy if env
   vars from `.env.local` aren't present on the host.

3. **Build provenance.** Should `pocopine deploy` refuse to
   deploy when `git status` is dirty? Proposal: **warn, don't
   refuse**. Passing `--dirty` suppresses the warning.

4. **Multi-target dev loop.** Can a project target two hosts
   simultaneously (e.g. CF Pages for the static preview,
   Shuttle for the API)? Proposal: yes — each adapter is
   independent; nothing in `.pocopine/deploy/` prevents having
   both `shuttle.toml` and `cf-pages.toml` alongside.

5. **CI integration.** The adapters assume interactive
   terminals (login prompts, etc.). GitHub Actions / GitLab
   CI / Vercel deploy webhooks all have non-interactive
   auth flows. Proposal: each adapter's README includes a CI
   snippet (GitHub Actions workflow that passes the host's
   `<X>_TOKEN` env var).

## 9. Alternatives considered

- **Build our own deploy service.** No — pocopine has no
  reason to operate infrastructure. The hosts above already
  solve hosting; we just wire the last-mile CLI.
- **Single universal "deploy anywhere" abstraction.** No — the
  hosts differ meaningfully (WASM MIME, bandwidth, region,
  secrets handling). Wrapping them behind one lowest-common-
  denominator API erases the reasons users pick a specific
  host. Thin per-target adapters keep the wrapper honest.
- **Generate a Nix flake / Earthfile / Dagger pipeline.** Over-
  engineered for where pocopine is today. Revisit if users
  start deploying to a dozen hosts at once.
