#!/usr/bin/env bash
set -euo pipefail

log_file="$(mktemp -t pocopine-otlp-smoke.XXXXXX.log)"
trap 'rm -f "$log_file"' EXIT

if cargo test -p observability-smoke --test otlp_smoke >"$log_file" 2>&1; then
  grep -E '^(running [0-9]+ test|test result:)' "$log_file" || true
else
  cat "$log_file"
  exit 1
fi
