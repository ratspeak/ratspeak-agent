#!/usr/bin/env bash
# Build Ratspeak Agent for the system this runs on, and copy its installers into build/dist/.
# The Pi release is embedded in the app, so scripts/package.sh runs first (it needs build/dist/ratspeak-pi-native
# from scripts/build-native.sh). Needs Rust and `cargo install tauri-cli`.
set -euo pipefail
cd "$(dirname "$0")/.."
PI=$(python3 -c "import ratspeak_pi; print(ratspeak_pi.VERSION)")
APP=$(grep -m1 '^version' app/src-tauri/Cargo.toml | cut -d'"' -f2)
[ "$PI" = "$APP" ] || { echo "build-app: versions differ (Pi $PI, app $APP)" >&2; exit 1; }
python3 scripts/check-upstream-pins.py
./scripts/build-desktop-helper.sh
./scripts/package.sh >/dev/null
(cd app/src-tauri && cargo test --release --locked && cargo tauri build -- --locked)
BUNDLE=${CARGO_TARGET_DIR:-app/src-tauri/target}/release/bundle
if command -v dpkg-deb >/dev/null && ls "$BUNDLE"/deb/*.deb >/dev/null 2>&1; then
  ./scripts/deb-version.sh "$BUNDLE"/deb/*.deb
fi

for made in "$BUNDLE"/macos/*.app "$BUNDLE"/dmg/*.dmg "$BUNDLE"/deb/*.deb "$BUNDLE"/rpm/*.rpm "$BUNDLE"/nsis/*.exe "$BUNDLE"/msi/*.msi; do
  if [ -e "$made" ]; then
    cp -R "$made" build/dist/
    echo "build/dist/$(basename "$made")"
  fi
done
