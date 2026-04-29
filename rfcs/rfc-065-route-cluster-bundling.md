# RFC 065 — Route-cluster bundling

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-29 |
| **Supersedes** | RFC 064 §4.1 (split out per council feedback 2026-04-29) |
| **Related** | [RFC 003](./rfc-003-router.md), [RFC 058 §5.10](./rfc-058-compiled-views-walker-removal.md), [RFC 060](./rfc-060-component-uses-registry.md), [RFC 061](./rfc-061-compiled-mount-only.md), [RFC 064](./rfc-064-performance-roadmap.md) |
| **Depends on** | RFC 060 implemented; baseline measurement of `examples/website` boot bundle |

## 1. Summary

Implement the route-boundary clustering committed by RFC 058
§5.10.1: per-route bundle splitting driven by the `uses` graph
RFC 060 already builds. The shell cluster ships at boot; each
route's cluster fetches on first navigation. **No author code
changes** — the split is inferred from `App::route::<C>(...)`
calls + `uses` declarations the macro already requires.

This is **not a runtime perf RFC** — it doesn't make any code
faster. It's a delivery-architecture RFC: same code, distributed
across smaller artifacts that load lazily. Council split it out
of RFC 064 because the concerns (artifact generation, cluster
loaders, route-load behavior) don't share a code surface with
RFC 064's runtime perf work and shouldn't share an RFC.

## 2. Motivation

Today every pocopine app ships every component every route
could possibly mount, in one wasm binary. For a counter
(1 component, 1 route) that's correct. For a website with 50
routes (login, marketing pages, dashboard, settings, billing,
admin, etc.) it's terrible — first-paint downloads code for
pages the user may never visit.

Vue 3 + React + Solid solve this with bundler-driven
code splitting (`React.lazy`, `defineAsyncComponent`, dynamic
import). Pocopine has the typed graph that makes the same
splitting compiler-driven: `App::route::<HomePage>("/")` plus
HomePage's `uses = [...]` plus each used component's transitive
`uses` give an exact cluster definition with no author hint
required.

The result: counter is unchanged (single route, nothing to
split). Website example's boot bundle drops by an amount that
depends on the route shape — measurement-gated per §3.

## 3. Baseline requirement

**This RFC is not actionable until a fresh baseline measurement
is captured.** Before the implementation PR opens, capture and
commit to `bench/baseline-clustering.md`:

- **`examples/website` boot bundle**: raw + gzip wasm size
  today (single-binary).
- **Per-route reachability map**: from each `App::route::<C>`
  entry, list the transitive `uses` closure. This becomes the
  cluster definition; the audit confirms it's small enough to
  matter.
- **Counter boot bundle** (control): unchanged; serves as the
  "one-route apps don't pay" verification.

Without these numbers, any "≥40% reduction" claim is
speculative. The phase delivery in §6 quotes the *measurement
discipline*, not an absolute target.

## 4. Non-goals

- **Not WIT-bindgen / Component Model split delivery.** RFC 058
  Phase 8 is the long-term direction; this RFC ships the
  per-cluster wasm *generation* and a fetch-driven loader, not
  Component Model artifacts. The fetch loader is the v1
  delivery; WIT-bindgen replaces it later without author code
  changes.
- **Not hand-tunable cluster boundaries.** v1 infers everything
  from `uses`. RFC 058 §5.10.1 mentions `#[component(cluster
  = "checkout")]` as a future affordance; this RFC defers
  manual boundaries.
- **Not SSR/hydration cluster delivery.** RFC 059 owns hydrate;
  cluster-aware hydration is a follow-up.
- **Not dynamic `App::mount_subtree::<C>` splitting.** Subtree
  mounts assume the typed `C` is already loaded — they happen
  in user code that already imports the type.

## 5. Design

### 5.1 Cluster definition

Given `App::route::<C1>(...)`, `App::route::<C2>(...)`, ...:

- **Shell cluster** = ⋂ (transitive `uses` closure of each
  route's root component). Components every route needs;
  ships at boot.
- **Route cluster N** = (transitive `uses` closure of route
  N's root) − shell cluster. Components only that route
  needs; ships on first navigation to that route.

The closure walk extends RFC 060 Tier 4's existing
infrastructure. Each cluster gets its own `&'static phf::Map`
literal at build time.

### 5.2 Artifact generation

The `app!{}` macro (RFC 060 Tier 4) gains a closure-walk pass
that:

1. Computes cluster membership per §5.1.
2. Emits one `&'static phf::Map` per cluster (shell + N route
   clusters).
3. Wraps each non-shell cluster's component code in a
   `#[link_section]` or equivalent mechanism that lets the
   build pipeline emit them as separate wasm modules.

The exact "separate wasm module" mechanism depends on
toolchain support. Three options, ordered by complexity:

- **Option A — single binary, lazy-init**: all clusters live in
  one wasm. The cluster loader is a no-op fetch (returns
  immediately). Buys nothing on first paint but proves the
  architecture before per-cluster artifacts exist. v1 ships
  this.
- **Option B — multiple wasm artifacts, fetch-loaded**: each
  non-shell cluster builds to its own `*.wasm`. The shell's
  cluster loader fetches the route's wasm on navigation,
  instantiates it, registers its components. v1.5 ships this
  once the cargo + wasm-pack story for multi-artifact builds is
  proven.
- **Option C — Component Model / WIT-bindgen**: each cluster is
  a wasm Component implementing a shared world interface (RFC
  058 Phase 8). Future direction; gated on toolchain.

This RFC ships **Option A first** as the architectural
commitment, with Option B as the delivery follow-up. Authors
write the same code either way; the linker/fetch behavior
differs.

### 5.3 Cluster loader runtime

New `crates/pocopine-core/src/clusters.rs`:

```rust
/// A cluster's static surface — shell loads at boot, route
/// clusters load on first navigation.
pub struct Cluster {
    pub name: &'static str,
    pub registry: &'static phf::Map<&'static str, &'static ComponentVTable>,
    pub init: fn(),  // calls register() on each component
}

/// Shell cluster — loaded by App::run_with_registry.
pub fn install_shell(shell: &'static Cluster);

/// Route cluster — loaded by router on navigation.
pub async fn ensure_route_cluster(route: &str) -> Result<(), ClusterLoadError>;
```

Option A's `ensure_route_cluster` is a synchronous no-op in
async clothing. Option B fills it in with `fetch()` +
`WebAssembly.instantiate()` + the cluster's `init()` call.

### 5.4 Router integration (RFC 003 touch)

`router::navigate(target)` becomes async-aware:

1. Determine the matched route's cluster.
2. `ensure_route_cluster(cluster_name).await`.
3. Mount the route's component into `<pp-outlet>`.

Option A: step 2 returns immediately. Option B: step 2 awaits
the fetch.

UI behavior during the fetch is its own concern — Vue 3's
`<Suspense>` + Solid's transition machinery solve this. v1
default: simple "loading…" placeholder; richer story in a
follow-on RFC.

### 5.5 Author-facing API

**Zero changes.** Authors continue to write:

```rust
App::new()
    .store::<Preferences>()
    .route::<HomePage>("/")
    .route::<SettingsPage>("/settings")
    .route::<BillingPage>("/billing")
    .run();
```

The `app!{}` macro (RFC 060 Tier 4) does the cluster walk; the
runtime does the lazy load; the author sees identical
ergonomics.

### 5.6 Subtree mounts and clusters

`App::mount_subtree::<C>(host)` requires `C` to be visible
in the calling code's module graph — i.e. the user already
imported it. Subtree mounts therefore implicitly belong to the
caller's cluster. v1 does not allow subtree-mounting a
component from a not-yet-loaded cluster; that's a future
affordance with its own ergonomics question (does
`mount_subtree` await? error? auto-fetch?).

## 6. Phased delivery

### 6.1 Phase 1 — Option A (single-binary cluster architecture)

**Deliverable**: cluster computation runs at build time;
shell + per-route clusters exist as separate `phf::Map`
literals; the cluster loader exists; routes go through the
loader (which is a no-op). The wasm binary is unchanged in
size and behavior; the *architecture* is in place.

**Phase delivery (gated)**:

- Counter behavior unchanged; counter binary size unchanged
  (single-route apps have an empty cluster split).
- Website example `App::run` produces the same DOM as today.
- Per-route cluster definitions match the §3 baseline's
  reachability map.
- All existing tests pass.

### 6.2 Phase 2 — Option B (multi-artifact + fetch loader)

**Prerequisites**:

- Phase 1 merged.
- `cargo build` + `wasm-pack` story for emitting multiple
  wasm artifacts from one `App::run` is reproducible; this
  may require a custom build helper.

**Deliverable**: per-route cluster wasm files emitted; the
shell wasm shrinks by the cluster sizes; navigation triggers
fetch + instantiate + registration before mount.

**Phase delivery (gated)**:

- `examples/website` shell wasm shrinks by a measurable
  amount vs the Phase 1 endpoint (target TBD post-baseline;
  RFC 058 §5.10 implied "≥40%" but that's a guess until §3
  baseline + per-route reachability lands).
- Per-route cluster wasm files exist on disk.
- Navigation latency for cluster load (cold) measured + below
  a chosen threshold (e.g. ≤500 ms on a 4G connection).
- Counter unchanged.
- All existing tests pass.

### 6.3 Phase 3 — refinements (separate RFC)

UX during cluster load (Suspense-style placeholders), error
recovery, prefetch hints, manual `cluster = "..."` overrides.
Each of these is its own follow-up.

## 7. Testing requirements

Per phase:

- **Phase 1**:
  - New test: cluster computation against a synthetic
    multi-route fixture matches a hand-rolled expected
    cluster definition.
  - Existing tests + counter + website example boot to the
    same DOM as today.
  - Twiggy: per-cluster `phf::Map` symbols exist in the
    binary.

- **Phase 2**:
  - Navigation test: visiting a route triggers a fetch for
    its cluster, then mounts.
  - Per-cluster wasm files are present in the build output.
  - Cold + warm navigation latency measured; warm is
    cache-served (≤10 ms).
  - Failed fetch surfaces a router error, not a panic.

## 8. Measurement requirements

Each PR includes in its body:

- `examples/website` shell wasm size delta.
- Per-cluster wasm file sizes.
- Navigation latency for cluster load (cold + warm cache),
  measured against a representative network profile.
- Counter wasm size (control — should not change).

The dashboard at `bench/dashboard.md` (shared with RFC 064)
gains rows for shell + per-cluster sizes.

## 9. Open questions

1. **Option B build mechanism** — `cargo build` doesn't
   natively emit multiple wasm artifacts from one binary
   target. Does this RFC require:
   - a custom `build.rs` helper that splits the linked wasm
     post-build? (Fragile; depends on linker section
     behavior.)
   - separate cargo targets per cluster? (Clean; but the macro
     needs to know cluster boundaries before cargo invokes
     it.)
   - `wasm-bindgen-cli`'s split-into-modules support
     (if it exists / is stable)?
   This is the biggest unknown and gates Phase 2.

2. **Cluster identity for `App::mount_subtree::<C>`** — same
   question as RFC 064 had: does subtree-mounted `C`
   participate in clustering, or is it shell-only? This RFC
   recommends shell-only for v1 (the `mount_subtree` API
   already requires the type to be in the caller's import
   graph), but the question reopens for tooling-mounted
   widgets in non-pocopine pages.

3. **Per-route cluster boundary on shared compounds** — when
   `PineDialog` is used by routes A + B but not C, it
   belongs to the shell (A ∩ B ⊂ shell? — no, only A ∩ B ∩
   C, which is wrong). The §5.1 definition needs sharpening:
   "components reachable from ≥2 routes" probably belong in
   the shell to avoid duplicating across clusters. Worth
   spec'ing precisely before Phase 1.

4. **Cluster loader async story** — `ensure_route_cluster`
   returns a Future. The router needs to handle the await.
   Today's router (RFC 003) is synchronous. Migrating to
   async-aware routing is a small but real touch on RFC 003.

## 10. Why this is enough

Per-route bundling is the difference between "pocopine ships a
website" and "pocopine ships *boot for the homepage*, then
fetches each page on demand." The author writes the same code;
the user downloads dramatically less on first visit.

Combined with RFC 064's runtime-perf work, the picture for a
realistic application is:

- **Counter** (1 route, no compounds): RFC 064 size wins, no
  RFC 065 effect — already optimal.
- **Pine showcase / website** (50 routes, heavy compound
  reuse): RFC 065 dominates — boot-bundle reduction is the
  headline number.
- **jsbench** (single fixture, lots of rows): RFC 064 §5.4
  dominates — reconcile speed is the headline number.

Three RFCs (064 + 065 + the existing benchmark-harness work)
together are what makes pocopine measurable against Solid /
Yew / Leptos on the dimensions each excels at.
