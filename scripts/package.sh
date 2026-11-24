#!/usr/bin/env bash
# Make the release a Pi owner downloads: build/dist/ratspeak-pi-<version>-aarch64.tar.gz and its .sha256.
# Needs build/dist/ratspeak-pi-native from scripts/build-native.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -x build/dist/ratspeak-pi-native ] || { echo "package: run scripts/build-native.sh first" >&2; exit 1; }
V=$(python3 -c "import ratspeak_pi; print(ratspeak_pi.VERSION)")
NAME=ratspeak-pi-$V-aarch64
STAGE=build/stage/$NAME
rm -rf build/stage && mkdir -p "$STAGE/bin"
cp install.sh firstboot.sh README.md LICENSE NOTICE "$STAGE/"
cp -R ratspeak_pi "$STAGE/" && find "$STAGE" -name __pycache__ -prune -exec rm -rf {} +
mkdir "$STAGE/runtime" && cp runtime/*.py runtime/LICENSE runtime/VENDORED.md "$STAGE/runtime/"
cp build/dist/ratspeak-pi-native "$STAGE/bin/"
COPYFILE_DISABLE=1 tar --no-xattrs -C build/stage -czf "build/dist/$NAME.tar.gz" "$NAME"  # no macOS metadata
(cd build/dist && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256" && cat "$NAME.tar.gz.sha256")
