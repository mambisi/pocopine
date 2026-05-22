# pocopine build container

The canonical, **opt-in** environment that compiles pocopine apps into
deployable artefacts (RFC 080 §4.4). Users opt in with
`pocopine build --container`; the default path uses the host's local
Rust toolchain.

## Image

```
ghcr.io/pocopine/build:<pocopine-cli-version>-rust-<rust-version>
```

Examples:

```
ghcr.io/pocopine/build:0.1.0-rust-1.84
ghcr.io/pocopine/build:0.1.0-rust-1.85    # bumped toolchain
ghcr.io/pocopine/build:0.2.0-rust-1.85    # bumped pocopine release
```

## Contents

- `rust ${RUST_VERSION}` with `wasm32-unknown-unknown` target
- `wasm-bindgen-cli`, `trunk`, `binaryen` (`wasm-opt`), `sccache`
- `pocopine` CLI built from this repo's source
- `clang`, `lld`, `libssl-dev`, `pkg-config`, `git`, `curl`, `nodejs`
- No host CLIs (no `flyctl`, `railway`, `gcloud`, …) — deploy happens
  outside the container after artefacts land on the host

## Build locally

```sh
docker buildx build \
  --build-arg RUST_VERSION=1.84 \
  --build-arg POCOPINE_REV=$(git rev-parse HEAD) \
  -t ghcr.io/pocopine/build:0.1.0-rust-1.84 \
  -f docker/build/Dockerfile .
```

## Use

```sh
# Default path — local Rust, no container:
pocopine build

# Opt in to this image (no rustup required on host):
pocopine build --container
# Equivalent to:
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/pocopine/build:<v> pocopine build --in-container
```

Output (`target/release/*`, `dist/`, `pocopine-build-meta.json`) lands
on the host filesystem via the bind mount and is picked up by
`pocopine deploy --target <host>`.

## Publish

CI workflow publishes a new image on every `pocopine-cli` release plus
on `rust:<version>-slim-bookworm` base-image security advisories. The
publish workflow lives under `.github/workflows/build-container.yml`
(to be added in a follow-up; for now, push manually after a verified
local build).

## Versioning policy

- **Major.minor.patch** matches `pocopine-cli`'s release version.
- **`-rust-<x.y>`** suffix matches the pinned toolchain.
- Bumping Rust is a **minor** bump on the image.
- Bumping `wasm-bindgen-cli` / `trunk` / `sccache` is a **patch** bump
  unless their major versions move.

`pocopine deploy doctor` warns when the running image is older than
what the current `pocopine-cli` recommends.
