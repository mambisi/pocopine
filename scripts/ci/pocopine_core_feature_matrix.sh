#!/usr/bin/env bash
set -euo pipefail

# Keep release-only cfgs honest on both host and wasm. `--all-targets`
# intentionally compiles integration tests too: instrumentation-backed tests
# must carry the same cfg as the instrumentation they exercise.
cargo check -p pocopine-core --release --all-targets --no-default-features
cargo check -p pocopine-core --release --all-targets --all-features
cargo check -p pocopine-core --release --target wasm32-unknown-unknown --all-targets --no-default-features
cargo check -p pocopine-core --release --target wasm32-unknown-unknown --all-targets --all-features
