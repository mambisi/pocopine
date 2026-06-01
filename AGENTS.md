# AGENTS.md

House rules for agents (Claude Code, Codex, …) working in this workspace.

## Encoding & cryptography: go through the shared crates

This workspace centralizes all hashing, checksums, encoding, and request
signing into two crates. **Do not** add `sha2` / `md-5` / `crc32c` / `hmac` /
`base64` / `percent-encoding` to a crate's `Cargo.toml`, and **do not**
hand-roll a percent-encoder, hex loop, or `Digest` accumulator. Use:

- **`pocopine-crypto`** — `sha256`/`sha256_hex`, `md5_hex`, `crc32c_hex`,
  `digest_hex(Algorithm, _)`, streaming `Hasher` (`finalize_hex` /
  `finalize_bytes`), and keyed `hmac_sha256(key, msg)`.
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

## Verify across targets before pushing

Most of these crates feed wasm bundles, and a host `cargo build`/`test` does
**not** surface the wasm32-cfg / `cargo fmt` issues CI enforces. Run both:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --target wasm32-unknown-unknown
cargo build --workspace --target wasm32-unknown-unknown
cargo test --workspace            # host
```
