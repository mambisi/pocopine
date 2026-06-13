# RFC 100 - Asset pipeline: content-addressed media from `assets/` to the edge

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-06-12 |
| **Related** | [`rfc-080-deploy-contract.md`](./rfc-080-deploy-contract.md) (deploy orchestration, credentials.toml, adapter API doctrine), [`rfc-082-pocopine-storage.md`](./rfc-082-pocopine-storage.md) (upload protocol — *not* this pipeline; see §3), [`rfc-099-ssr-hydration.md`](./rfc-099-ssr-hydration.md) (SSR serves the same URLs), `docs/internal/roadmap-0.2.x.md` (0.2.1 publishing pipeline) |

Overview diagram: [`docs/internal/asset-pipeline.png`](../docs/internal/asset-pipeline.png).

## 1. Summary

Apps need images, video, and fonts that are **not** user uploads:
marketing media, blog clips, UI sounds. Today those either bloat the
wasm (`include_bytes!`), ride as unversioned static files (cache
poison on every edit), or get hand-uploaded to a CDN with hand-edited
URLs. This RFC gives pocopine one opinionated pipeline:

1. **Convention** — media lives in `assets/` at the crate root,
   referenced from Rust as `asset!("blog/clip.webm")`.
2. **Content addressing** — the macro hashes the file at build time;
   every URL embeds the hash (`/assets/<hash8>/<path>`), so every
   object is immutable and cacheable for a year.
3. **One write path** — `pocopine assets push` hash-diff-syncs
   `assets/` to an S3-compatible bucket; `pocopine deploy` runs the
   same sync *before* the app flips.
4. **Two serve modes, no menu beyond them** — a public bucket/CDN
   base (Mode A) or a private bucket proxied by the web service
   (Mode B). The URL shape never changes between dev, Mode A, and
   Mode B — only the base does.

```text
                 BUILD                       DEPLOY                     SERVE
 assets/blog/clip.webm                 pocopine assets push        dev:    /assets/<h>/.. ← dev server (verify, 409 on stale)
        │  asset!("blog/clip.webm")      hash-diff sync            Mode A: <CDN base>/<h>/.. ← public bucket/CDN
        ▼  hashes bytes → a3f81c2d           │                     Mode B: /assets/<h>/.. ← web service ──S3──▶ private bucket
 asset_url("blog/clip.webm","a3f81c2d")      ▼
        │  base resolved at RUNTIME    bucket key assets/a3f81c2d/blog/clip.webm
        ▼                              content-type: video/webm
 "<base>/a3f81c2d/blog/clip.webm"      cache-control: public,max-age=31536000,immutable
```

## 2. The `assets/` convention

* `assets/` at the **calling crate's** root
  (`$CARGO_MANIFEST_DIR/assets/`). Relative paths inside it become
  URL paths verbatim, so they must be UTF-8; the sync skips dotfiles
  (`.DS_Store` and friends).
* Not served as plain static files in production: every production
  URL goes through a content hash. (The dev server still falls
  through to plain `assets/<path>` for hand-written URLs; that is a
  dev convenience, not a contract.)
* `assets/` is `.gitignore`-able where files are build products
  (e.g. the website's Remotion renders); committed where they are
  sources. The pipeline doesn't care — it hashes whatever is there.

## 3. Not pocopine-storage

RFC-082 is the **user-upload** protocol: authenticated sessions,
policies, scopes, resumable uploads, multiple backends. Assets are
the opposite shape: developer-owned, build-time-known, immutable,
public-ish. They share nothing but S3 wire compatibility, so the
asset pipeline gets its own thin client (`pocopine-assets`, §5) and
deliberately does **not** ride `StorageBackend`. One consequence,
recorded: `pocopine-storage` depends on `pocopine-server` (the
upload protocol uses its axum surface), so the asset client must be
a **leaf crate** below `pocopine-server` — it cannot live in
`pocopine-storage-s3` without a dependency cycle.

## 4. The `asset!` macro — hash at build, base at runtime

```rust
let url: String = asset!("blog/clip.webm");
// dev    → "/assets/a3f81c2d/blog/clip.webm"
// Mode A → "https://assets.example.com/a3f81c2d/blog/clip.webm"
// Mode B → "/assets/a3f81c2d/blog/clip.webm"   (same as dev; the proxy serves it)
```

Expansion-time semantics (all are **compile errors** on violation):

* the argument is a string literal, resolved against
  `$CARGO_MANIFEST_DIR/assets/<path>`;
* path rules: non-empty, relative, no `..`, no root/drive prefixes
  (`./` segments are tolerated); the file must exist and be readable;
* the bytes are hashed — **the asset hash is the first 8 lowercase
  hex chars of the sha256 digest** (via `pocopine-crypto`), the
  single hash shape used everywhere in this RFC;
* expansion: `::pocopine::__private::asset_url("<path>", "<hash8>")`
  (plus the rebuild-tracking const of §6 when built through the CLI).

`asset_url` (in `pocopine-core::assets`) joins
`<base>/<hash>/<path>` resolving the base **at runtime** on every
call:

* wasm32 — `window.__POCOPINE_ASSET_BASE` when a non-empty string;
* native/SSR — the `POCOPINE_ASSET_BASE` env var when non-empty;
* default — `/assets`. Trailing slashes on a configured base are
  trimmed.

Hashing at build and basing at runtime is the load-bearing split:
the same compiled artefact serves dev, SSR, and a CDN-fronted deploy
— promoting an app from Mode B to Mode A is an env-var change, not a
rebuild.

### 4.1 Dev server contract (the `409`)

`pocopine dev`/`run` (static mode) own the asset route:

* `GET /assets/<hash8>/<path>` → serve `assets/<path>` from the
  project with `Cache-Control: public,max-age=31536000,immutable`
  and the MIME table's content type — **after verifying** the hash
  against the file bytes;
* hash matches → `200`; hash differs → **`409 Conflict`** with an
  explanation: the binary was compiled against different bytes
  (asset edited without a recompile — possible under bare cargo,
  §6); the body names the actual hash and the rebuild fix;
* URLs that don't parse as asset URLs (no 8-lower-hex segment) or
  whose file is missing fall through to the normal static handler.

The 409 exists because dev serves from the **mutable working tree**;
it is the backstop when rebuild tracking isn't armed. Production
never verifies (§8.1).

## 5. One write path

### 5.1 The client: `pocopine-assets`

A leaf crate (aws-sdk-s3 + tracing only) exposing `AssetStore`
— `ensure_bucket` / `list_keys` / `put` / `get` — used by **both**
the CLI sync and the Mode B proxy. One client type is what
guarantees the write and read paths can never disagree about keys,
content types, or cache headers. Works against any S3-compatible
endpoint (Railway Buckets, R2, AWS S3, MinIO); custom endpoints are
addressed path-style.

### 5.2 Keys, headers, MIME

* Key: `assets/<hash8>/<relative-path>` — content-addressed, never
  overwritten with different bytes by construction.
* `Cache-Control: public,max-age=31536000,immutable` on every
  object (`pocopine_assets::ASSET_CACHE_CONTROL`).
* Content type from **one** MIME table
  (`pocopine-cli::assets_sync::mime_of`), shared verbatim with the
  dev server route; the Mode B proxy echoes the stored content type,
  so all three serve paths agree. Browsers genuinely need this:
  Chromium refuses muted-autoplay `<video>` on
  `application/octet-stream`.

### 5.3 The sync (`pocopine assets push`)

```text
scan assets/ ──▶ [ (rel, hash8) … ]            (sorted, dotfiles skipped)
list bucket keys under assets/  ──▶ existing
upload every assets/<hash8>/<rel> ∉ existing   (content-type + immutable)
print: N uploaded, M skipped, B transferred
```

* Idempotent: pushing an unchanged tree uploads nothing.
* An edited file gets a **new** key; the old key stays (still-cached
  HTML may reference it). Garbage collection of unreferenced hashes
  is a non-goal for v1 — storage is cheap, correctness isn't.
* Old keys are never mutated, so a crashed sync cannot corrupt
  anything; re-running completes the diff.
* Config: `[package.metadata.pocopine.assets]` in the project's
  `Cargo.toml` (RFC-080 §4.1 note applies — Pocopine.toml is the
  long-term home):

| key | meaning |
|---|---|
| `endpoint` | S3-compatible endpoint URL; **omit for AWS S3** (SDK derives it from `region`). Custom endpoints → path-style addressing |
| `bucket` | bucket name (required) |
| `region` | signing region; optional, default `us-east-1` (what MinIO/R2-`auto`/Railway accept) |
| `public-base` | public/CDN base URL over the bucket; **presence selects Mode A** (deploy sets `POCOPINE_ASSET_BASE` to it); absence selects Mode B |

* Credentials: the `[assets]` entry in `~/.pocopine/credentials.toml`
  (`pocopine assets auth`, mode 0600, RFC-080 §5.5 machinery), with
  `POCOPINE_ASSETS_ACCESS_KEY_ID` / `POCOPINE_ASSETS_SECRET_ACCESS_KEY`
  env override for CI (both-or-neither; a half-set pair is an error).
  Per the standing rule, these are **deploy-time** credentials —
  app runtime secrets never enter pocopine state. (The same env names
  double as the Mode B proxy's runtime credentials on the host, set
  by the host's secret store — the *names* are shared, the storage
  is not.)

## 6. Rebuild correctness — the fingerprint env

The macro bakes the hash at expansion time, and asset files are
invisible to cargo's dependency tracking (`include_bytes!` would fix
that by embedding the media in the rlib — rejected). The fix:

1. **CLI side** — `pocopine build`/`run`/`dev`/`deploy` compute a
   combined fingerprint of `assets/` (sorted relative paths +
   per-file sha256, hashed together via `pocopine-crypto::Hasher`)
   and export it as `POCOPINE_ASSETS_FINGERPRINT` on **every** cargo
   and wasm-pack invocation. No `assets/` directory → env unset.
2. **Macro side** — when the env is set at expansion time, `asset!`
   additionally emits, inside its expression block:
   `const _: Option<&str> = ::core::option_env!("POCOPINE_ASSETS_FINGERPRINT");`
   rustc records every `option_env!` expansion as an env dependency
   in the crate's dep-info (`# env-dep:` lines) and cargo folds the
   value into the unit fingerprint — so an asset edit changes the
   env, dirties the calling crate, recompiles it, and **re-expands**
   `asset!` with the fresh hash.

The mechanism is **proven**, not assumed:
`crates/pocopine-cli/tests/fingerprint_tracking.rs` builds a
dependency-free proc macro emitting exactly this construct, flips
the env between builds with no source change, and asserts the
tracked consumer re-expands while an identical *untracked* control
crate stays stale — demonstrating both that the tracking works and
that the emitted const is load-bearing.

Deliberate gating, recorded: the const is emitted **only when the
env is set**, so bare-cargo / rust-analyzer builds (env unset)
record no env-dep and don't rebuild-ping-pong against CLI builds.
The cost: tracking only arms once the crate compiles through the
pocopine CLI, and a crate last built by bare cargo can hold a stale
hash — the dev server's 409 (§4.1) is the backstop. `pocopine dev`
also watches `assets/` and rebuilds wasm + server bins on changes,
closing the loop live (an `assets/` directory created mid-session
needs a dev restart to be watched).

## 7. Atomicity — sync before flip

`pocopine deploy` runs the asset sync **before** the app flip
(before `adapter.deploy`, in both normal and `--skip-build` runs):
new keys land beside old ones (content-addressed keys cannot
collide), then the app — and with it the new URLs — goes live. A
failed sync aborts the deploy with the old app still serving the old
keys. There is no window where a live app references a missing
asset, and no ordering knob to misconfigure.

## 8. Serving — two modes

### Mode A — public bucket / CDN

The bucket (or a CDN over it) is publicly readable.
`public-base` in config → deploy injects
`POCOPINE_ASSET_BASE=<public-base>` into the service env (explicit
`[deploy.env]` declarations win) → `asset_url` emits absolute CDN
URLs. The web service never touches asset bytes. This is the
graduation target: R2/S3 + CDN, browser-to-edge.

### Mode B — private bucket proxy

**Railway Buckets are private-only** — there is no public-read
toggle — so Mode A is impossible there. Instead asset URLs stay on
the app's origin (default `/assets` base, same as dev) and the web
service proxies:

```text
browser ── GET /assets/a3f81c2d/blog/clip.webm ──▶ web service
                                                      │ S3 GET (internal network)
                                                      ▼
                                      bucket key assets/a3f81c2d/blog/clip.webm
```

This is economically sound on Railway specifically because
service-egress is free there and the bucket sits on the internal
network; the browser-visible response still carries the immutable
cache header, so each client fetches each asset once.

The proxy lives in `pocopine-server` and installs **automatically**
in `Server::new` when the env contract is present — zero code in the
app's `main`, no opt-in API. Env contract:

| env var | meaning |
|---|---|
| `POCOPINE_ASSETS_BUCKET` | bucket name; presence (non-empty) enables the route |
| `POCOPINE_ASSETS_ENDPOINT` | S3-compatible endpoint URL (omit for AWS S3) |
| `POCOPINE_ASSETS_REGION` | signing region, default `us-east-1` |
| `POCOPINE_ASSETS_ACCESS_KEY_ID` | static access key id (required once bucket is set) |
| `POCOPINE_ASSETS_SECRET_ACCESS_KEY` | static secret access key (required once bucket is set) |

Route semantics (`GET /assets/<hash8>/<*path>`):

* hash segment must be exactly 8 lowercase hex chars, else `404`
  without touching the bucket;
* found → `200` with the stored content type
  (octet-stream fallback) and stored cache-control (immutable
  fallback); missing key → `404`; bucket failure → `502` (logged to
  `pocopine.log`); bucket configured but keys missing → the route
  answers `503` naming the missing env var (fail loud, boot anyway).

### 8.1 Why the proxy does NOT verify hashes (and dev does)

The dev server re-hashes file bytes per request because it serves
the mutable working tree, which can drift from the hash compiled
into the binary. In Mode B **the bucket key is the hash**: the sync
computed it from the exact bytes it uploaded, and keys are immutable
by convention. Re-hashing every response would buffer and burn CPU
to compare the store against itself; a URL whose hash has no key
simply 404s. Different trust model, deliberately different contract.

## 9. Zero-config tier and graduation

**Zero config (Railway):** the Railway adapter provisions a Railway
Bucket through the host API on first deploy, stores the issued
access keys in `credentials.toml` (CLI side, for `assets push`),
sets the `POCOPINE_ASSETS_*` service env vars through the adapter
API (runtime side — Mode B only, since Railway buckets are private),
and leaves `POCOPINE_ASSET_BASE` at the `/assets` default. A user
who creates `assets/`, writes `asset!`, and deploys to Railway gets
working immutable media with **zero** configuration. *(Adapter
auto-provisioning is specced here, not yet implemented — §12.)*

**Graduation (R2 / S3 / MinIO):** two config lines —

```toml
[package.metadata.pocopine.assets]
endpoint = "https://<account>.r2.cloudflarestorage.com"
bucket   = "myapp-assets"
# + public-base = "https://assets.example.com"  → Mode A
```

— plus `pocopine assets auth`. **URLs never change shape** across
dev → Railway → R2: always `<base>/<hash8>/<path>`. Nothing in the
app, templates, or HTML rewrites.

## 10. Non-goals (v1, binding)

1. **No image optimization service** — no resizing/transcoding/
   `srcset` generation. Hash-and-serve only; optimization is a
   build-side concern users own.
2. **No per-host static hosting** — assets go to one S3-compatible
   bucket, not to N host-specific static-file products (per the
   no-extension-protocols rule: small enumerable surface, PRs
   welcome).
3. **No LFS / large-file source management** — `assets/` is plain
   files; how they get into the tree is out of scope.
4. **`.poco` `$asset()` syntax is phase 2** — v1 is Rust-side only
   (`asset!` into a field/prop, bound in the template). Decided
   syntax: **`$asset('path')`** — a pine-expr intrinsic that
   constant-folds to a string literal at template-compile time
   (hash baked, missing file = compile error). Valid wherever
   pine-expr evaluates: bound attributes
   (`:src="dark ? $asset('a.svg') : $asset('b.svg')"`), plain
   quoted attributes (build-time substitution,
   `src="$asset('blog/intro.webm')"`), and `{{ }}` text
   interpolation (`<p>{{ $asset('report.pdf') }}</p>`, with
   interpolation's existing direct-text-children limits). Both
   quote styles accepted, single preferred inside double-quoted
   attributes. Binding grammar rule, which killed the unquoted
   variant: **`.poco` must always parse as valid HTML** — syntax
   extensions live only inside attribute values and `{{ }}` text
   nodes, never in markup structure.
5. **No bucket GC** — unreferenced old hashes are kept (see §5.3).
6. **Streaming/Range in the Mode B proxy** — v1 buffers full bodies
   and answers `Range` requests with a `200` full body (HTTP
   permits this). Documented follow-up: a streaming read on
   `pocopine-assets`, then `206` support in the proxy. Fine for
   site media; revisit before anyone serves feature-length video.

## 11. Parity duplication (recorded debt)

The 8-hex sha256-prefix hash is implemented in **three** crates —
`pocopine-core::assets::asset_hash` (runtime), `pocopine-macros`
(expansion), `pocopine-cli::assets_sync` (dev server + sync) — all
four-liners over `pocopine_crypto::sha256_hex`, each pinned to the
same known vectors (`e3b0c442`/`b94d27b9`) in unit tests, plus the
end-to-end parity assertion in `crates/pocopine/tests/asset_macro.rs`.
A proc-macro crate cannot link the wasm runtime crate, which forces
at least two copies. **Open question:** hoist the helper into a
shared no_std crate (`pocopine-crypto` itself is the natural home —
an `asset_hash` there would collapse all three) versus leaving three
pinned four-liners. Deferred until the hash shape has survived a
release.

## 12. Implementation state (at writing)

| piece | state |
|---|---|
| `asset!` + `asset_url` + dev 409 route | landed |
| fingerprint env (CLI export + macro tracking + proof test) | landed |
| `pocopine-assets` store; `pocopine assets push` / `auth`; credentials `[assets]` entry | landed |
| deploy wiring: sync-before-flip; Mode A `POCOPINE_ASSET_BASE` env injection | landed |
| Mode B proxy in `pocopine-server` (env-enabled, auto-installed) | landed |
| MinIO integration tests (store ops; end-to-end push twice + edit) | landed |
| Railway adapter bucket auto-provisioning (§9) | **follow-up** |
| `window.__POCOPINE_ASSET_BASE` injection into served `index.html` (Mode A in-browser base) | **follow-up** — until then Mode A is fully effective for SSR-emitted URLs; pure-client apps on Mode A need the global set in their `index.html` |
| streaming/Range proxy reads | follow-up (§10.6) |
| `.poco` `$asset("path")` syntax | phase 2 (§10.4) |
