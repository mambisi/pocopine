#!/usr/bin/env bash
# jsbench — bench + profile driver for the per-framework harnesses.
#
# Usage:
#   ./benchmark.sh [<harness>] [--browser <name>] [options]
#   ./benchmark.sh --all [--browser <name>] [options]
#
# Harnesses (each `jsbench/<name>/`):  pocopine | leptos | yew | vue
# Browsers:                            firefox (default) | chromium | webkit
#
# Modes (mutually exclusive, default is bench):
#   --bench             Bench plan: 2 warmups + 5 measured runs per
#                       action. Mean / p50 / p95 / max wall-clock.
#   --profile           Single runLots(10000) with the pocopine
#                       mount-profiler enabled; prints the profile
#                       JSON. Pocopine harness only.
#   --profile-bench     Full bench plan + per-action profile. Prints
#                       the slowest run's phase breakdown alongside
#                       the wall-clock distribution. Pocopine only.
#
# Other:
#   --all               Run the chosen mode against every harness.
#   --no-build          Skip the wasm-pack rebuild for Rust harnesses.
#   -h | --help         This help text.
#
# Examples:
#   ./benchmark.sh                            # bench pocopine, firefox
#   ./benchmark.sh leptos --browser chromium  # bench leptos, V8
#   ./benchmark.sh --all --browser chromium   # all four, V8
#   ./benchmark.sh --profile                  # mount profile pocopine
#   ./benchmark.sh --profile-bench            # full-plan profile pocopine

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ALL_HARNESSES=(pocopine leptos yew vue)

harness=""
browser="firefox"
mode="bench"
build=1
run_all=0

usage() { sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --browser)        browser="$2"; shift 2 ;;
    --bench)          mode="bench"; shift ;;
    --profile)        mode="profile-mount"; shift ;;
    --profile-bench)  mode="profile-bench"; shift ;;
    --all)            run_all=1; shift ;;
    --no-build)       build=0; shift ;;
    -h|--help)        usage; exit 0 ;;
    --*)              echo "unknown flag: $1" >&2; exit 2 ;;
    *)
      if [[ -n "$harness" ]]; then
        echo "harness already set to '$harness', got '$1'" >&2
        exit 2
      fi
      harness="$1"; shift ;;
  esac
done

case "$mode" in
  profile-mount|profile-bench)
    if [[ "$run_all" -eq 1 ]]; then
      echo "--profile / --profile-bench only apply to the pocopine harness" >&2
      exit 2
    fi
    if [[ -z "$harness" ]]; then
      harness="pocopine"
    elif [[ "$harness" != "pocopine" ]]; then
      echo "--profile / --profile-bench require harness=pocopine (got $harness)" >&2
      exit 2
    fi
    ;;
esac

# Build a Rust harness if needed. Vue / vanilla harnesses are
# plain JS — nothing to compile.
build_harness() {
  local name="$1"
  local dir="${HERE}/${name}"
  if [[ ! -f "${dir}/Cargo.toml" ]]; then return 0; fi
  if [[ "$build" -eq 0 ]]; then return 0; fi

  pushd "${dir}" >/dev/null
  if [[ "$mode" != "bench" && "$name" == "pocopine" ]]; then
    wasm-pack build --release --target web --features mount-profiler
  else
    wasm-pack build --release --target web
  fi
  popd >/dev/null
}

# Run measure.py for one harness, dispatching on $mode.
run_harness() {
  local name="$1"
  local dir="${HERE}/${name}"
  if [[ ! -d "${dir}" ]]; then
    echo "no harness at ${dir}" >&2
    exit 1
  fi
  build_harness "${name}"

  local extra=()
  case "$mode" in
    profile-mount) extra=(--profile-mount) ;;
    profile-bench) extra=(--profile-bench) ;;
  esac

  printf '\n══ %s — %s — %s ══\n' "${name}" "${browser}" "${mode}"
  python3 "${HERE}/measure.py" "${extra[@]}" --browser "${browser}" "${dir}"
}

if [[ "$run_all" -eq 1 ]]; then
  for h in "${ALL_HARNESSES[@]}"; do
    run_harness "$h"
  done
else
  run_harness "${harness:-pocopine}"
fi
