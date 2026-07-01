# Network Tool

Give an AI agent **bounded, auditable outbound HTTP**: fetch one allowlisted URL
and get back clean, paginated text — without ever handing it a path to internal
services, cloud metadata, or the host's private network.

This is the second-highest-risk tool family after `process`. Where the process
tool treats the **OS sandbox** as the trust boundary, the network tool treats the
**SSRF guard (resolve → validate → pin)** as the trust boundary, and everything
else is defence in depth.

---

## Tool surface

| Tool id | Purpose | Side effect | Default policy |
|---|---|---|---|
| `net.fetch` | GET one allowlisted URL → bounded, paginated markdown/text | side-effecting | opt-in (needs a `NetPolicy` + the network capability) |

`net.fetch` is **not** in any default tool set: it is registered explicitly
against a host `NetPolicy`. With no allowlist it fetches nothing.

> **Also built:** the subprocess **egress proxy** ([`EgressProxy`](#egress-proxy-for-subprocesses)) — its
> CONNECT-authorization core is done; wiring it into the process sandbox is the
> one remaining step (see *Roadmap*).
>
> **Deferred / not yet built** (see *Roadmap*): `net.download` (waits on the
> artifacts tool), `net.resolve`, robots.txt, the URL-in-context anti-exfiltration
> control, and a response cache.

---

## Quick start

```rust
use agenkitty::tools::{register_network_tools, NetPolicy};

// Opt in a set of public documentation domains (https/443 by default).
let policy = NetPolicy::allow(["docs.rs", "developer.mozilla.org"]);
let agenkit = register_network_tools(Agenkit::builder().provider(p), policy)?
    .build()?;
```

A model invokes it like any tool:

```json
{ "url": "https://docs.rs/tokio", "max_length": 5000 }
```

and gets structured output:

```jsonc
{
  "final_url": "https://docs.rs/tokio/latest/tokio/",
  "status": 200,
  "content_type": "text/html; charset=utf-8",
  "content": "# Tokio\n\n…markdown…",
  "truncated": true,
  "next_start_index": 5000          // pass back as start_index for the next page
}
```

---

## The SSRF guard (the trust boundary)

Every request — and every redirect hop — passes through one `guard` entrypoint
that implements **resolve → validate → pin**:

```
  url ─▶ parse + canonicalize (WHATWG)         reject userinfo / non-http(s) /
        normalize host (IDNA, lowercase,       >250 chars / odd IP encodings
        trailing-dot)                          internal-name rules
              │
              ▼
        resolve ONCE (hickory)  ─▶  validate EVERY resolved A/AAAA against the
              │                      CIDR deny-list (::ffff: → v4 first)
              ▼
        PIN the socket to the validated IPs (reqwest resolve_to_addrs);
        hostname kept only for TLS SNI. The client NEVER re-resolves.
              │
              ▼  every redirect hop re-runs the whole pipeline
          connect
```

Pinning is what defeats **DNS rebinding** (the dominant SSRF bypass): the socket
can only reach an address that was already validated, so a record that flips to
an internal IP after validation is never connected to.

The CIDR deny-list is deliberately broader than "RFC1918 + metadata" — it
includes CGNAT, TEST-NETs, reserved/deprecated ranges, and the IPv6 transition
forms (NAT64, 6to4, Teredo, IPv4-mapped, IPv4-compatible) that 2025–26 SSRF
advisories exploited. Internal hostnames (`localhost`, `*.internal`, `*.lan`,
`metadata.google.internal`, k8s service names, …) are blocked before DNS.

---

## What's enforced

| Concern | Mechanism |
|---|---|
| SSRF / DNS rebinding | resolve → validate every resolved IP → pin the socket; re-run on every redirect hop. |
| Private / metadata targets | hard-coded IPv4 **and** IPv6 CIDR deny-list + internal-name rules. |
| Allowlist | `NetPolicy.allowed_domains` — exact + **label-boundary** suffix (never substring), case-insensitive, IDNA-normalized; mutually exclusive with `blocked_domains`; deny-by-default. |
| Scheme / port | https-only on port 443 by default (`with_schemes` / `with_ports` to widen). |
| Redirects | followed manually (reqwest's follower disabled), re-guarded each hop, capped at 5; `final_url` reflects the last hop. |
| Encoding bypasses | WHATWG parser rejects decimal/octal/hex IP literals; userinfo rejected; host IDNA + case + trailing-dot normalized before any check. |
| Response size | streamed and capped (5 MB default, configurable) in the read loop — `Content-Length` is never trusted. |
| Decompression bombs | gzip/deflate/brotli decompressed transparently, but the read-loop cap applies to the **decompressed** stream (lazy — pulling stops at the cap), so it doubles as the bomb ceiling. |
| Content type | `text/*`, `text/html`, `application/json` only; HTML → markdown; binary rejected (`unsupported_content_type`). |
| Budgets | connect 5 s / total 30 s timeouts; optional per-registration request budget (`max_requests`). |
| Secrets | credential headers (`Authorization`, …) carried as `SecretString`, sent **only to the origin host**, dropped on cross-host redirect, never echoed to output/traces/logs. |
| Observability | one `tracing` event per call (`target: "pocopine.log"`, RFC-069) with host + status + truncation only — never the URL, headers, or body. |

**Secure by default:** `NetPolicy::default()` denies every host; a host opts in
specific domains.

---

## Configuration (`NetPolicy`)

```rust
let policy = NetPolicy::allow(["docs.rs"])      // or NetPolicy::new(allow, block)
    .with_schemes(["https"])                    // default: https
    .with_ports([443])                          // default: 443
    .with_max_response_bytes(5 * 1024 * 1024)   // default: 5 MB (decompressed)
    .with_max_requests(100);                    // default: unlimited
```

Credential headers are injected on the tool, never via the model's JSON:

```rust
use pocopine_agenkit::server::SecretString;
use reqwest::header::AUTHORIZATION;

let tool = NetFetchTool::new(policy)?
    .with_secret_header(AUTHORIZATION, SecretString::new(token));
```

---

## Egress proxy (for subprocesses)

`net.fetch` covers "read this URL". The long tail of subprocess egress —
`cargo`/`npm`/`pip`/`git`/`curl` run by the process tool — is covered by
[`EgressProxy`], a host-side **HTTP CONNECT** proxy that authorizes every tunnel
against the **same** `NetPolicy` + SSRF guard, then tunnels TLS opaquely
(hostname/SNI only, no interception — like Claude Code's proxy):

```rust
use agenkitty::tools::network::EgressProxy;

let proxy = EgressProxy::new(NetPolicy::allow(["crates.io", "static.crates.io"]))?;
let (addr, _task) = proxy.bind_local().await?;     // 127.0.0.1:<port>
// inject into the sandboxed child's env:
//   HTTP_PROXY  = http://<addr>
//   HTTPS_PROXY = http://<addr>
```

A CONNECT to an allowlisted host on an allowed port whose resolved IP passes the
SSRF deny-list gets `200 Connection established` and is tunnelled to the
**pinned** address; anything else gets `403 Forbidden`. This subprocess egress
and `net.fetch` share one policy + one guard.

**Status — two enforcement tiers:**

- **Tier 1 (built):** `HTTP_PROXY` injection. `ProcessToolConfig::egress_proxy` /
  `ProcessRunTool::with_egress_proxy(addr)` set `HTTP_PROXY`/`HTTPS_PROXY`/… on the
  sandboxed child (host config overrides caller env; via `--setenv` under
  bubblewrap). Routes well-behaved clients (cargo/npm/pip/git) — the baseline
  Claude Code documents for its builtin proxy. A non-cooperating binary with INET
  access can still open a direct socket.
- **Tier 2 (built):** true bypass-prevention — parity with Codex
  (bwrap+seccomp+proxy) / Claude Code (bwrap+socat). Set
  `ProcessToolConfig::egress_proxy_uds` (or `SandboxPolicy::with_egress_proxy_uds`)
  to a UDS the proxy serves (`EgressProxy::bind_uds`). The child then runs under
  bwrap `--unshare-net` (no internet route at all) with that UDS bound in, wrapped
  by an **in-namespace relay shim** (`agenkitty __egress-shim`): the shim runs a
  loopback→UDS relay, exports `HTTP_PROXY=http://127.0.0.1:<p>`, and execs the real
  command — so *nothing but the allowlisted proxy is reachable*, even for a
  non-cooperating binary. `deny_inet()` keeps loopback INET open here; the
  **namespace**, not seccomp, is the egress control.

  The relay, supervisor, arg-builder, and the shim CLI are unit-tested + smoke-
  tested on the host. The bwrap-namespace integration itself must be validated on a
  real Linux+bubblewrap host (this repo's bwrap tests skip where namespaces are
  unavailable) before relying on Tier 2 for untrusted workloads.

---

## Module layout

| File | Responsibility |
|---|---|
| `mod.rs` | module wiring + public re-exports |
| `ssrf.rs` | the guard: CIDR deny-list, `validate_url`, `normalize_host`, `resolve_and_pin`, `guard`, the injectable `Resolve` trait, sealed `ValidatedTarget` |
| `policy.rs` | `NetPolicy` — allow/block-list, scheme/port, size/request budgets |
| `fetch.rs` | `net.fetch` — `authorize` (policy + guard), pinned client, redirect loop, stream-and-cap, HTML→markdown, pagination, credential headers, tracing; the production `HickoryResolver` |
| `proxy.rs` | `EgressProxy` — host-side CONNECT proxy giving subprocesses allowlisted egress through the same policy + guard |
| `error.rs` | `NetError`/`NetErrorCode` taxonomy → `AgenkitError` at the boundary |
| `registry.rs` | `register_network_tools(builder, NetPolicy)` + tool-id helpers |

---

## Errors

Fine-grained `NetErrorCode` (mapped to `AgenkitError` kinds; the code stays in the
message for the model): `invalid_url`, `url_too_long`, `url_not_allowed`
(allowlist / private target / scheme / port), `url_not_accessible` (DNS / connect
/ HTTP / timeout), `unsupported_content_type`, `too_many_requests`.

---

## Testing

`cargo test -p agenkitty tools::network` — unit tests, including a DNS-rebinding
simulation (mock `Resolve`r), the full IPv4+IPv6 deny-list, allowlist
label-boundary matching, pagination, the redirect decision, the request budget,
and origin-host credential gating. The live HTTP round-trip is intentionally not
unit-tested (the guard blocks loopback, so a local server is unreachable through
the real path); the security-critical logic is covered by the pure/injected-DNS
tests, and the HTTP plumbing is thin glue over `reqwest`.

---

## Roadmap (deferred)

- **`net.download`** — bounded binary → the artifact store (content-hashed).
  Waits on the artifacts tool (currently plan-only).
- **`net.resolve`** — host-enabled DNS/URL metadata.
- **robots.txt**, **URL-in-context anti-exfiltration**, **response cache**.

> Egress proxy (both tiers, incl. the Tier-2 namespace shim) is **built**; the
> bwrap-namespace integration needs validation on a Linux+bwrap host.

[`EgressProxy`]: #egress-proxy-for-subprocesses
