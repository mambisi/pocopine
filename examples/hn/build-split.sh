#!/usr/bin/env bash
set -euo pipefail

mode="${1:-release}"
case "$mode" in
  release) profile_flag="--release" ;;
  dev) profile_flag="--dev" ;;
  *) echo "usage: $0 [release|dev]" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

build_one() {
  local out_name="$1"
  local features="$2"
  wasm-pack build \
    --target web \
    --no-opt \
    --out-dir pkg \
    --out-name "$out_name" \
    "$profile_flag" \
    "$root" \
    --no-default-features \
    --features "$features"
}

build_one hn shell
build_one hn_route_list route-list
build_one hn_route_detail route-detail
build_one hn_route_not_found route-not-found

cat >"$root/pkg/hn-split-manifest.json" <<'JSON'
{
  "shell": "/pkg/hn_bg.wasm",
  "routes": {
    "/": "/pkg/hn_route_list_bg.wasm",
    "/item/:id": "/pkg/hn_route_detail_bg.wasm",
    "*": "/pkg/hn_route_not_found_bg.wasm"
  }
}
JSON
