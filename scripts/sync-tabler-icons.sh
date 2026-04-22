#!/usr/bin/env bash
# Vendor Tabler Icons (https://github.com/tabler/tabler-icons) into
# crates/pine-icons/assets/tabler/. Writes MANIFEST.json pinning
# the upstream commit so rebuilds are deterministic.
#
# Usage:
#   scripts/sync-tabler-icons.sh                   # main
#   scripts/sync-tabler-icons.sh v3.17.0           # tag
#   scripts/sync-tabler-icons.sh abcdef0           # sha / branch
#
# Requires: bash 4+, git, coreutils. No jq / python.

set -euo pipefail

REF="${1:-main}"

# Resolve repo root relative to the script's own location so the
# script works regardless of the shell's current directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEST="$REPO_ROOT/crates/pine-icons/assets/tabler"
LICENSE_TARGET="$REPO_ROOT/crates/pine-icons/LICENSE.tabler-icons"

# Refuse to run on a dirty tree under assets/ — prevents losing
# uncommitted experimental edits under the vendored tree.
if ! git -C "$REPO_ROOT" diff --quiet -- "$DEST" 2>/dev/null \
 || ! git -C "$REPO_ROOT" diff --cached --quiet -- "$DEST" 2>/dev/null; then
    echo "error: uncommitted changes under $DEST — commit or stash first." >&2
    exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "==> cloning tabler-icons @ $REF"
git clone --depth 1 --branch "$REF" \
    https://github.com/tabler/tabler-icons.git "$TMP" 2>&1 \
    | sed 's/^/    /'

SHA="$(git -C "$TMP" rev-parse HEAD)"

echo "==> copying SVGs into $DEST"
mkdir -p "$DEST/outline" "$DEST/filled"
# Drop existing vendored SVGs before copy — guards against stale
# icons lingering after an upstream rename.
rm -f "$DEST/outline/"*.svg "$DEST/filled/"*.svg 2>/dev/null || true
cp "$TMP"/icons/outline/*.svg "$DEST/outline/"
cp "$TMP"/icons/filled/*.svg  "$DEST/filled/"

# Strip the leading `<!-- tags: … -->` metadata block Tabler ships
# in every file. It's useful context upstream but wasted bytes once
# vendored — each SVG gets `include_str!`'d into WASM, so the saved
# ~150 bytes per icon × 5000 icons matters.
echo "==> stripping upstream metadata comments"
for svg in "$DEST/outline/"*.svg "$DEST/filled/"*.svg; do
    if head -n 1 "$svg" | grep -q '^<!--'; then
        # Delete from line 1 through first closing `-->`. Keeps the
        # subsequent `<svg …>` element intact.
        sed -i.bak '1,/-->/d' "$svg" && rm -f "$svg.bak"
    fi
done

# First-run or upstream-updated license copy. Tabler ships MIT.
if [ -f "$TMP/LICENSE" ]; then
    cp "$TMP/LICENSE" "$LICENSE_TARGET"
fi

OUT_COUNT="$(find "$DEST/outline" -maxdepth 1 -name '*.svg' -type f | wc -l | tr -d ' ')"
FIL_COUNT="$(find "$DEST/filled"  -maxdepth 1 -name '*.svg' -type f | wc -l | tr -d ' ')"
NOW="$(date -u +%FT%TZ)"

# Hand-rolled JSON so we don't need jq in CI / developer machines.
cat > "$DEST/MANIFEST.json" <<EOF
{
  "source": "https://github.com/tabler/tabler-icons",
  "ref": "$REF",
  "commit": "$SHA",
  "fetched_at": "$NOW",
  "license": "MIT",
  "counts": {
    "outline": $OUT_COUNT,
    "filled": $FIL_COUNT
  }
}
EOF

echo
echo "  outline icons: $OUT_COUNT"
echo "  filled icons:  $FIL_COUNT"
echo "  commit:        $SHA"
echo "  manifest:      $DEST/MANIFEST.json"
echo
echo "==> git status $DEST"
git -C "$REPO_ROOT" status --short "$DEST" | sed 's/^/    /' || true
