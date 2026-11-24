#!/usr/bin/env bash
# Build the amd64 Linux helper used by desktop containers. ARM64 uses the Pi release.
set -euo pipefail
cd "$(dirname "$0")/.."
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/rsReticulum" "$stage/rsLXMF" "$stage/rsLXST" "$stage/pi/runtime" build/dist
for spec in rsReticulum:RETICULUM_REV rsLXMF:LXMF_REV rsLXST:LXST_REV; do
  repo=${spec%:*}
  rev=$(awk -F'"' -v key="${spec#*:}" '$1 == key " = " {print $2}' runtime/common.py)
  git -C "../$repo" archive "$rev" | tar -xf - -C "$stage/$repo"
done
cp -R runtime/native "$stage/pi/runtime/"
# Build with the native CPU and cross-link on ARM to avoid emulating rustc.
tar -cf - -C "$stage" . | docker run --rm -i rust:1.96.0-trixie sh -ec '
  mkdir /src && tar -xf - -C /src
  if [ "$(uname -m)" != x86_64 ]; then
    apt-get update -qq >&2 && apt-get install -y -qq gcc-x86-64-linux-gnu libc6-dev-amd64-cross >&2
    rustup target add x86_64-unknown-linux-gnu >&2
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc
  fi
  cd /src/pi/runtime/native && cargo build --release --locked --target x86_64-unknown-linux-gnu >&2
  gzip -c target/x86_64-unknown-linux-gnu/release/agent-rat-native
' > "$stage/helper.gz"
gzip -t "$stage/helper.gz"
mv "$stage/helper.gz" build/dist/desktop-native-amd64.gz
