---
name: codec-crypto
description: >-
  Use when adding or touching any hashing, checksums, base64, percent-encoding
  / URL escaping, or request signing (HMAC, Azure Shared-Key / SAS, token
  hashing) in the pocopine workspace. Routes the work through the shared
  pocopine-crypto and pocopine-codec crates instead of pulling in sha2 / md-5 /
  crc32c / hmac / base64 / percent-encoding directly or hand-rolling encoders.
---

# Encoding & crypto: use the shared crates

The workspace has two centralizing crates. **Never** reach for the underlying
primitives or hand-roll an encoder/decoder/hash loop — add to (or call) these:

- **`pocopine-crypto`** — hashing, checksums, keyed MACs. Wraps `sha2`,
  `md-5`, `crc32c`, `hmac`. `no_std + alloc`.
- **`pocopine-codec`** — base64 + percent-encoding (+ serde adapters). Wraps
  `base64`, `percent-encoding`. `no_std + alloc`.

Both are safe for wasm crates. Add with `{ workspace = true }`. (Integration
tests reach a crate's normal `[dependencies]`, so they don't need a dev-dep.)

## What to call instead of what

| You need…                              | Use                                                                          | Don't                                  |
| -------------------------------------- | ---------------------------------------------------------------------------- | -------------------------------------- |
| base64 encode / decode                 | `pocopine_codec::base64_encode` / `base64_decode`                            | the `base64` crate / `STANDARD` engine |
| `Vec<u8>` serde field as base64        | `#[serde(with = "pocopine_codec::base64_bytes")]`                            | a bespoke serde adapter                |
| percent-encode a URL component         | `pocopine_codec::percent_encode(s)` / `percent_encode_into(&mut out, s)`     | a `%{:02X}` byte loop / `HEX` table    |
| percent-encode with a backend set      | `pocopine_codec::percent_encode_set(s, &SET)` — build `SET` from the re-exported `CONTROLS` / `NON_ALPHANUMERIC` / `AsciiSet` | `use percent_encoding::…`              |
| percent-decode                         | `pocopine_codec::percent_decode(s, plus_as_space)`                           | a hand-rolled `%XX` decoder            |
| sha256 / md5 / crc32c **hex**          | `pocopine_crypto::sha256_hex` / `md5_hex` / `crc32c_hex` / `digest_hex(alg)` | `sha2` / `md-5` / `crc32c` directly    |
| streaming hash of a large/streamed body| `pocopine_crypto::Hasher::new(alg)` + `update` + `finalize_hex`/`finalize_bytes` | a manual `Digest` accumulator       |
| raw sha256 `[u8; 32]`                  | `pocopine_crypto::sha256(bytes)`                                             | `Sha256::digest(...)`                  |
| HMAC-SHA256 (request signing)          | `pocopine_crypto::hmac_sha256(key, msg)` then base64/hex the `[u8; 32]`      | `hmac::Hmac<Sha256>` + `sha2`          |

`percent_encode` uses the RFC 3986 *unreserved* set (`A-Za-z0-9-._~`,
uppercase hex, non-ASCII always escaped) — the right default for path
segments, query keys/values, and fragments. Reach for `percent_encode_set`
only when a provider mandates a different set (e.g. the GCS JSON object-name
set, or SAS values needing the strict `NON_ALPHANUMERIC` set).

## The API is the contract — extend it, don't bypass it

If a primitive you need isn't wrapped yet (a new digest, a different MAC, a new
codec), **add it to the shared crate** and call that, rather than depending on
the raw crate from a consumer. Each crate re-exports its underlying crates
(`pocopine_crypto::{sha2, md5, crc32c, hmac}`, `pocopine_codec::{base64,
percent_encoding}`) as an escape hatch — prefer growing the API over using them.

## Genuine exceptions (leave as-is)

- **`argon2`** (password hashing → PHC strings) is intentionally *not* in
  `pocopine-crypto`, which is plain digests/checksums/MACs. Keep `argon2`
  direct in `pocopine-auth-credentials`.
- **`pocopine-core`'s `router/return_to.rs` `percent_decode_strict`** is a
  *strict, reject-on-malformed, reject-non-UTF-8* decoder for a security path.
  It is deliberately bespoke; `pocopine_codec::percent_decode` is lossy.

## Verify before pushing

These crates feed wasm bundles. Run both host and wasm checks (host
`cargo test` does **not** catch wasm32-cfg or fmt issues CI enforces):

```
cargo test -p pocopine-codec -p pocopine-crypto
cargo build -p pocopine-codec -p pocopine-crypto --target wasm32-unknown-unknown
cargo fmt --all -- --check && cargo clippy --workspace --target wasm32-unknown-unknown
```
