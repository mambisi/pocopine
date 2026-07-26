#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Match CI's host-test strictness by default. Callers can override with:
#   RUSTFLAGS= scripts/runtests.sh
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"

# Keep the local wrapper conservative by default. The full workspace graph is
# large, and several integration packages pull heavy dev-dependency trees even
# when service-backed tests are skipped.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

run() {
  printf '\n+'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

run_test_packages() {
  local label="$1"
  shift
  local package

  echo
  echo "== $label =="

  for package in "$@"; do
    run cargo test -p "$package" --tests
  done
}

contains_test() {
  local needle="$1"
  shift
  local item

  for item in "$@"; do
    [[ "$item" == "$needle" ]] && return 0
  done

  return 1
}

skip_reason() {
  local package="$1"
  local test_name="$2"
  local key="$package:$test_name"

  local docker_tests=(
    "pocopine:jobs_redis"
    "pocopine-assets:minio_assets"
    "pocopine-cli:assets_push_minio"
    "pocopine-storage-azure:azurite"
    "pocopine-storage-gcs:fake_gcs"
    "pocopine-storage-s3:minio"
  )
  local localhost_tests=(
    "pocopine-agenkit:oauth"
    "pocopine-agenkit-anthropic:contract"
    "pocopine-agenkit-anthropic:wire"
    "pocopine-agenkit-oai:contract"
    "pocopine-agenkit-oai:wire"
    "pocopine-auth-jwt:jwks_resolver_wiremock"
    "pocopine-auth-oauth:flow"
    "pocopine-collab:gateway"
    "pocopine-deploy-cloudflare-pages:cloudflare_http"
    "pocopine-deploy-railway:railway_http"
    "pocopine-realtime:gateway"
    "pocopine-server:server_plugin"
    "pocopine-storage:tus_js_client_e2e"
  )
  local live_tests=(
    "pocopine-deploy-railway:spec_drift"
  )
  local wasm_browser_tests=(
    "pocopine-storage:client_browser"
  )

  if contains_test "$key" "${docker_tests[@]}" && [[ "${POCOPINE_RUN_DOCKER_TESTS:-0}" != "1" ]]; then
    echo "set POCOPINE_RUN_DOCKER_TESTS=1 to run Docker-backed tests"
    return 0
  fi

  if contains_test "$key" "${localhost_tests[@]}" && [[ "${POCOPINE_RUN_LOCAL_NET_TESTS:-0}" != "1" ]]; then
    echo "set POCOPINE_RUN_LOCAL_NET_TESTS=1 to run localhost-backed tests"
    return 0
  fi

  if contains_test "$key" "${live_tests[@]}" && [[ "${POCOPINE_RUN_LIVE_TESTS:-0}" != "1" ]]; then
    echo "set POCOPINE_RUN_LIVE_TESTS=1 to run live network tests"
    return 0
  fi

  if contains_test "$key" "${wasm_browser_tests[@]}"; then
    echo "run wasm-pack test --firefox --headless crates/pocopine-storage --features test-utils --test client_browser for browser tests"
    return 0
  fi

  return 1
}

run_integration_tests() {
  local package="$1"
  local test_dir="$2"
  local test_file test_name reason

  for test_file in "$test_dir"/*.rs; do
    [[ -e "$test_file" ]] || continue
    test_name="$(basename -- "$test_file" .rs)"
    if reason="$(skip_reason "$package" "$test_name")"; then
      echo
      echo "Skipping $package $test_name integration test; $reason."
      continue
    fi
    run cargo test -p "$package" --test "$test_name"
  done
}

run_lib_package() {
  local package="$1"
  local test_dir="$2"
  run cargo test -p "$package" --lib
  run_integration_tests "$package" "$test_dir"
}

echo "Running pocopine host test suite"
echo "RUSTFLAGS=${RUSTFLAGS:-<empty>}"
echo "CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS"
echo "RUST_TEST_THREADS=$RUST_TEST_THREADS"
echo "CARGO_INCREMENTAL=$CARGO_INCREMENTAL"

# The macro crate has trybuild tests that spawn nested cargo builds and also
# dev-depend on the runtime crate. Keep it in its own target dir, matching CI,
# so broad workspace feature unification cannot race local crate metadata.
run env CARGO_TARGET_DIR=target/sync-query-macros-host \
  cargo test -p pocopine-sync-query-macros --tests

# Host workspace tests. Keep wasm-only/browser-only examples out of this pass;
# they are covered by wasm/browser jobs and fail on host for legitimate cfg
# reasons. Packages with local service dependencies are run below so those
# individual tests can be opt-in without skipping the rest of the package.
#
# Do not collapse these back to one `cargo test --workspace --tests` command:
# that shape builds too much of the workspace graph in one Cargo invocation and
# has a much higher peak memory footprint on local machines.
run_test_packages "core and auth crates" \
  pocopine-analytics \
  pocopine-auth \
  pocopine-auth-client \
  pocopine-auth-credentials \
  pocopine-codec \
  pocopine-core \
  pocopine-crypto \
  pocopine-events \
  pocopine-expr \
  pocopine-jobs \
  pocopine-live \
  pocopine-logging \
  pocopine-observe

run_test_packages "macro and codegen crates" \
  pocopine-agenkit-core \
  pocopine-agenkit-macros \
  pocopine-client-codegen \
  pocopine-directives \
  pocopine-macros \
  pocopine-template-parser \
  pocopine-ts-rs \
  pocopine-ts-rs-macros

run_test_packages "pine UI support crates" \
  pine-charts \
  pine-icons \
  pine-icons-macros \
  pine-layout \
  pine-motion \
  pine-richtext \
  pine-richtext-collab

run_test_packages "sync crates" \
  pocopine-sync \
  pocopine-sync-indexdb \
  pocopine-sync-query \
  pocopine-sync-sqlite

run_test_packages "tooling crates" \
  gen-model-catalog \
  pocopine-deploy \
  pocopine-deploy-render \
  pocopine-launcher \
  pocopine-native \
  pocopine-native-tauri \
  pocopine-stylekit

run_test_packages "examples" \
  blog \
  charts \
  collab-canvas \
  counter \
  file-browser-example \
  hn \
  jsbench \
  jsbench-leptos \
  jsbench-yew \
  keep-example \
  layout-demo \
  live-example \
  observability-frontend \
  richtext \
  site \
  spa \
  tags_input_repro \
  tailwind-demo \
  todo \
  website \
  workspace-demo

run_lib_package pocopine-agenkit crates/pocopine-agenkit/tests
run_lib_package pocopine-agenkit-anthropic crates/pocopine-agenkit-anthropic/tests
run_lib_package pocopine-agenkit-oai crates/pocopine-agenkit-oai/tests
run_lib_package pocopine-assets crates/pocopine-assets/tests
run_lib_package pocopine-auth-jwt crates/pocopine-auth-jwt/tests
run_lib_package pocopine-auth-oauth crates/pocopine-auth-oauth/tests
run_lib_package pocopine-collab crates/pocopine-collab/tests
run_lib_package pocopine-deploy-cloudflare-pages crates/pocopine-deploy-cloudflare-pages/tests
run_lib_package pocopine-deploy-railway crates/pocopine-deploy-railway/tests
run_lib_package pocopine-realtime crates/pocopine-realtime/tests
run_lib_package pocopine-server crates/pocopine-server/tests
run_lib_package pocopine-storage crates/pocopine-storage/tests
run cargo test -p pocopine-storage --features image-compression --lib --test storage_host
run_lib_package pocopine-storage-azure crates/pocopine-storage-azure/tests
run_lib_package pocopine-storage-gcs crates/pocopine-storage-gcs/tests
run_lib_package pocopine-storage-s3 crates/pocopine-storage-s3/tests

if [[ "${POCOPINE_RUN_LOCAL_NET_TESTS:-0}" == "1" ]]; then
  run cargo test -p pocopine-cli --bins
else
  run cargo test -p pocopine-cli --bins -- \
    --skip configured_port_check_reports_busy_server_port \
    --skip configured_port_check_ignores_static_mode
fi
run_integration_tests pocopine-cli crates/pocopine-cli/tests

run cargo test -p pocopine --lib
run_integration_tests pocopine crates/pocopine/tests
