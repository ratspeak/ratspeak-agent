#!/usr/bin/env bash
# Copy the Agent Tunnel helper (Python controller + Rust transport source) into runtime/ and record its commit.
# Apply the desktop/Pi admission integration before publishing any refreshed source.
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=${1:-../agent-tunnel}
root=$PWD
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/runtime/native/src"
for f in "$SRC"/runtime/*.py "$SRC"/runtime/LICENSE; do [ "$(basename "$f")" = install.py ] || cp "$f" "$stage/runtime/"; done
cp "$SRC"/runtime/native/{Cargo.toml,Cargo.lock} "$stage/runtime/native/"
cp "$SRC"/runtime/native/src/main.rs "$stage/runtime/native/src/"
(cd "$stage" && git apply --check "$root/scripts/agent-inbox.patch" && git apply "$root/scripts/agent-inbox.patch")
{
  echo "# Vendored helper"
  echo
  echo "Copied from Agent Tunnel by \`scripts/vendor.sh\`. The agent receive integration is applied from \`scripts/agent-inbox.patch\`."
  echo
  echo "- Source commit: $(git -C "$SRC" rev-parse HEAD)$(git -C "$SRC" diff --quiet -- runtime || echo ' (with uncommitted changes)')"
  echo "- Helper version: $(grep -m1 '^VERSION' "$SRC"/runtime/common.py | cut -d'"' -f2)"
  echo "- Licence: runtime/LICENSE (Reticulum License)"
} > "$stage/runtime/VENDORED.md"
cp -R "$stage/runtime/." runtime/
cat runtime/VENDORED.md
