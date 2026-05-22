# syntax=docker/dockerfile:1.7
#
# pocopine canonical build container — RFC 080 §4.4
#
# Tag convention: <pocopine-cli-version>-rust-<rust-version>
#   e.g. ghcr.io/pocopine/build:0.1.0-rust-1.84
#
# Build:
#   docker buildx build \
#     --build-arg RUST_VERSION=1.84 \
#     --build-arg POCOPINE_REV=$(git rev-parse HEAD) \
#     -t ghcr.io/pocopine/build:0.1.0-rust-1.84 \
#     -f docker/build/Dockerfile .
#
# Use:
#   docker run --rm -v "$PWD:/workspace" -w /workspace \
#     ghcr.io/pocopine/build:<v> pocopine build --in-container

# ─── Stage 1 — build pocopine-cli from this repo's source ────────────────────
ARG RUST_VERSION=1.84
FROM rust:${RUST_VERSION}-slim-bookworm AS cli-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates clang lld \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN cargo build --release -p pocopine-cli --bin pocopine

# ─── Stage 2 — the canonical build image ─────────────────────────────────────
FROM rust:${RUST_VERSION}-slim-bookworm AS build

ARG POCOPINE_REV=unknown
LABEL org.opencontainers.image.source="https://github.com/mambisi/pocopine" \
      org.opencontainers.image.description="pocopine canonical build container (RFC 080 §4.4)" \
      org.opencontainers.image.revision="${POCOPINE_REV}"

ENV CARGO_TERM_COLOR=always \
    CARGO_INCREMENTAL=0 \
    POCOPINE_BUILD_CONTAINER=1

# System deps for common Rust crates + wasm tooling.
# Pinned to bookworm so transitive package versions don't drift on rebuild.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        ca-certificates \
        curl \
        git \
        clang \
        lld \
        binaryen \
        nodejs \
    && rm -rf /var/lib/apt/lists/*

# WASM target + tooling. Versions are pinned; bump in lockstep with
# crates/pocopine-client-codegen so artefacts produced inside the container
# match what `pocopine build --no-container` produces locally.
RUN rustup target add wasm32-unknown-unknown
RUN cargo install --locked wasm-bindgen-cli@0.2.95 \
 && cargo install --locked trunk@0.21.7 \
 && cargo install --locked sccache@0.8.2

# sccache makes repeated `cargo build` calls fast across `docker run`
# invocations when the host mounts a cache volume at /sccache.
ENV RUSTC_WRAPPER=sccache \
    SCCACHE_DIR=/sccache

# Drop pocopine-cli built in stage 1.
COPY --from=cli-builder /src/target/release/pocopine /usr/local/bin/pocopine

WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/pocopine"]
CMD ["build", "--in-container"]
