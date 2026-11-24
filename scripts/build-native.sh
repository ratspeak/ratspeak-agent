#!/usr/bin/env bash
# Build the helper's Rust transport for 64-bit Raspberry Pi OS, so nothing compiles on the Pi.
# It is built natively in an arm64 Debian 12 container (the base of Raspberry Pi OS Bookworm), against the
# exact rsReticulum / rsLXMF / rsLXST commits the helper pins. Output: build/dist/ratspeak-pi-native (glibc 2.36+).
# A static musl build would need no container, but rsReticulum v1.4.0 does not compile for musl (rnsh ioctl type).
set -euo pipefail
cd "$(dirname "$0")/.."
RUST=1.96.0
RNS=$(grep -m1 '^RETICULUM_REV' runtime/common.py | cut -d'"' -f2)
LXMF=$(grep -m1 '^LXMF_REV' runtime/common.py | cut -d'"' -f2)
LXST=$(grep -m1 '^LXST_REV' runtime/common.py | cut -d'"' -f2)
SRC="$PWD/build/src"
BIN=$(grep -m1 '^name' runtime/native/Cargo.toml | cut -d'"' -f2)
rm -rf "$SRC" && mkdir -p "$SRC/rsReticulum" "$SRC/rsLXMF" "$SRC/rsLXST" "$SRC/pi/runtime" build/dist
git -C ../rsReticulum archive "$RNS" | tar -x -C "$SRC/rsReticulum"
git -C ../rsLXMF archive "$LXMF" | tar -x -C "$SRC/rsLXMF"
git -C ../rsLXST archive "$LXST" | tar -x -C "$SRC/rsLXST"
cp -R runtime/native "$SRC/pi/runtime/native"
# The workspace drive is not shared with Docker: sources go in and the binary comes out over pipes.
tar -C "$SRC" -cf - . | docker run --rm -i --platform linux/arm64 debian:bookworm-slim sh -c '
  set -e
  mkdir /src && tar -xf - -C /src
  apt-get update -qq >/dev/null && apt-get install -y -qq curl ca-certificates gcc libc6-dev >/dev/null 2>&1
  curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain '"$RUST"' >/dev/null 2>&1
  . "$HOME/.cargo/env"
  cargo build --release --locked --manifest-path /src/pi/runtime/native/Cargo.toml >&2
  cat /src/pi/runtime/native/target/release/'"$BIN" > build/dist/ratspeak-pi-native
chmod +x build/dist/ratspeak-pi-native
file build/dist/ratspeak-pi-native
