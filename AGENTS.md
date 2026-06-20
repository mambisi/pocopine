# AGENTS.md

House rules for agents (Claude Code, Codex, …) working in this workspace.

## Encoding & cryptography: go through the shared crates

This workspace centralizes all hashing, checksums, encoding, and request
signing into two crates. **Do not** add `sha2` / `md-5` / `crc32c` / `hmac` /
`base64` / `percent-encoding` to a crate's `Cargo.toml`, and **do not**
hand-roll a percent-encoder, hex loop, or `Digest` accumulator. Use:

- **`pocopine-crypto`** — `sha256`/`sha256_hex`, `md5_hex`, `crc32c_hex`,
  `digest_hex(Algorithm, _)`, streaming `Hasher` (`finalize_hex` /
  `finalize_bytes`), keyed `hmac_sha256(key, msg)`, and **`SecretString`** (an
  in-memory secret: redacted `Debug`/`Display`, zeroized on drop, read only via
  `expose()`). Don't add `zeroize` or hand-roll a redacted secret type — route
  every API key / token / password through `SecretString`.
- **`pocopine-codec`** — `base64_encode` / `base64_decode`, the
  `base64_bytes` serde adapter, `percent_encode` / `percent_encode_into`,
  `percent_encode_set(_, &AsciiSet)` for provider-specific sets, and
  `percent_decode(_, plus_as_space)`.

Both crates are `no_std + alloc`, so wasm crates (`pocopine-core`, the client
side) can depend on them. Add deps with `{ workspace = true }`.

If a primitive isn't wrapped yet, **extend the shared crate** (and call that)
rather than depending on the raw crate from a consumer. The full cheat-sheet,
the call-this-not-that table, and the two genuine exceptions (`argon2` for
password hashing; the strict `percent_decode_strict` in
`pocopine-core/src/router/return_to.rs`) live in the
[`codec-crypto` skill](.claude/skills/codec-crypto/SKILL.md). See also the
crate-level docs in `crates/pocopine-crypto/src/lib.rs` and
`crates/pocopine-codec/src/lib.rs`.

## Async store traits: one boxed-future shape, two Send-classes

The workspace has several **store** traits (`SessionStore`, `OAuthTokenStore`,
`AgentThreadStore`, `CollabStore`, `SyncLocalStore`, …). They are deliberately
**separate** — their keys, payloads, and write-ops genuinely diverge (replace vs
monotonic-snapshot vs append+owner vs multi-op), so there is no single
`SessionStore<T>` and you should not try to unify them. What *is* shared is the
async return shape; keep it to one of two forms (don't invent a third, and don't
pull in `futures` just to alias a one-liner):

- **Host store traits** → `Pin<Box<dyn Future<Output = T> + Send + 'a>>`. Each
  crate aliases this with a domain name (`AuthFuture`, agenkit `BoxFuture`,
  `StoreFuture`) — reuse the crate's existing alias; bake the crate's `Result`
  into `T` for ergonomics where the crate already does.
- **Wasm/client store traits** → the same **without `Send`** (single-threaded;
  `JsValue` isn't `Send`). `SyncLocalFuture` is the canonical example.

`#[async_trait]` is an acceptable alternative where a crate already uses it
(`pocopine-collab`, `pocopine-auth-credentials`); don't migrate those to manual
boxing. New `*Store` traits: copy the nearest existing alias, don't hand-roll a
new shape.

## Verify across targets before pushing

Most of these crates feed wasm bundles, and a host `cargo build`/`test` does
**not** surface the wasm32-cfg / `cargo fmt` issues CI enforces. Run both:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --target wasm32-unknown-unknown
cargo build --workspace --target wasm32-unknown-unknown
cargo test --workspace            # host
```
